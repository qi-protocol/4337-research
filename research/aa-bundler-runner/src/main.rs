mod utils;
use aa_bundler_grpc::{
    bundler_client::BundlerClient, bundler_service_run, uo_pool_client::UoPoolClient,
    uopool_service_run,
};
use aa_bundler_primitives::{Chain, UoPoolMode, Wallet};
use aa_bundler_rpc::{
    debug_api::{DebugApiServer, DebugApiServerImpl},
    eth_api::{EthApiServer, EthApiServerImpl},
    web3_api::{Web3ApiServer, Web3ApiServerImpl},
    JsonRpcServer,
};
use anyhow::{format_err, Result};
use dotenv::dotenv;
use env_logger::Env;
use ethers::{
    providers::{Http, Middleware, Provider},
    types::{Address, U256},
};
use std::env;
use std::net::SocketAddr;
use std::net::{IpAddr, Ipv4Addr};
use std::{collections::HashSet, future::pending, panic, sync::Arc};
use tracing::info;

use pin_utils::pin_mut;
use std::{future::Future, str::FromStr};

/// Parses address from string
pub fn parse_address(s: &str) -> Result<Address, String> {
    Address::from_str(s).map_err(|_| format!("String {s} is not a valid address"))
}

/// Parses U256 from string
pub fn parse_u256(s: &str) -> Result<U256, String> {
    U256::from_str_radix(s, 10).map_err(|_| format!("String {s} is not a valid U256"))
}

/// Parses UoPoolMode from string
pub fn parse_uopool_mode(s: &str) -> Result<UoPoolMode, String> {
    UoPoolMode::from_str(s).map_err(|_| format!("String {s} is not a valid UoPoolMode"))
}

/// Runs the future to completion or until:
/// - `ctrl-c` is received.
/// - `SIGTERM` is received (unix only).
pub async fn run_until_ctrl_c<F, E>(fut: F) -> Result<(), E>
where
    F: Future<Output = Result<(), E>>,
    E: Send + Sync + 'static + From<std::io::Error>,
{
    let ctrl_c = tokio::signal::ctrl_c();

    let mut stream = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let sigterm = stream.recv();
    pin_mut!(sigterm, ctrl_c, fut);

    tokio::select! {
        _ = ctrl_c => {
            info!("Received ctrl-c signal.");
        },
        _ = sigterm => {
            info!("Received SIGTERM signal.");
        },
        res = fut => res?,
    }

    Ok(())
}
fn main() -> Result<()> {
    env_logger::Builder::from_env(
        Env::default().default_filter_or("info"), // .default_filter_or("trace")
    )
    .init();

    dotenv().ok();
    let eth_client_address = env::var("HTTP_RPC").expect("Client not set");
    let mnemonic_file = env::var("MNEMONIC_FILE_PATH").expect("Mnemonic file not set");
    let bundler_address = env::var("BUNDLER_ADDRESS").expect("Bundler address not set");

    std::thread::Builder::new()
        .stack_size(128 * 1024 * 1024)
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_stack_size(128 * 1024 * 1024)
                .build()?;

            let task = async move {
                info!("Starting ERC-4337 AA Bundler");

                let eth_client =
                    Arc::new(Provider::<Http>::try_from(eth_client_address.clone()).unwrap());
                info!(
                    "Connected to the Ethereum execution client at {}: {}",
                    eth_client_address.clone(),
                    eth_client.client_version().await?
                );

                let chain_id = eth_client.get_chainid().await?;
                let chain = Chain::from(chain_id);

                let wallet = Wallet::from_file(mnemonic_file.clone().into(), &chain_id, None)
                    .map_err(|error| format_err!("Could not load mnemonic file: {}", error))?;
                info!("{:?}", wallet.signer);

                info!("Starting uopool gRPC service...");
                uopool_service_run(
                    // uopool service listen address
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 3001),
                    // entry points
                    vec!["0x5FF137D4b0FDCD49DcA30c7CF57E578a026d2789"
                        .parse::<Address>()
                        .unwrap()],
                    // execution client
                    eth_client.clone(),
                    // chain
                    chain,
                    // max verification gas
                    U256::from(1500000),
                    // minimum stake
                    U256::from(1),
                    // minimum unstake delay
                    U256::from(0),
                    // minimum priority fee
                    U256::from(0),
                    // whitelist address
                    vec![bundler_address.parse::<Address>().unwrap()],
                    // uopool mode
                    UoPoolMode::Standard,
                )
                .await?;
                info!("Started uopool gRPC service at {:}", "127.0.0.1:3001",);

                info!("Connecting to uopool gRPC service");
                let uopool_grpc_client =
                    UoPoolClient::connect(format!("http://{}", "127.0.0.1:3001",)).await?;
                info!("Connected to uopool gRPC service");

                info!("Starting bundler gRPC service...");
                bundler_service_run(
                    // bundler service listen address
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 3002),
                    // wallet
                    wallet,
                    // entry points
                    vec!["0x5FF137D4b0FDCD49DcA30c7CF57E578a026d2789"
                        .parse::<Address>()
                        .unwrap()],
                    // execution client rpc endpoint
                    eth_client_address.clone(),
                    // chain
                    chain,
                    // beneficiary
                    bundler_address.parse::<Address>().unwrap(),
                    // gas factor
                    U256::from(1),
                    // bundler minimum balance
                    U256::from(0),
                    // bundling interval
                    10u64,
                    uopool_grpc_client.clone(),
                );
                info!("Started bundler gRPC service at {:}", "127.0.0.1:3002",);

                info!("Starting bundler JSON-RPC server...");
                tokio::spawn({
                    async move {
                        let api: HashSet<String> =
                            HashSet::from_iter(vec!["eth".to_string(), "debug".to_string()]);

                        let mut server = JsonRpcServer::new("127.0.0.1:3000".to_string())
                            .with_proxy(eth_client_address.clone())
                            .with_cors(vec!["*".to_string()]);

                        server.add_method(Web3ApiServerImpl {}.into_rpc())?;

                        if api.contains("eth") {
                            server.add_method(
                                EthApiServerImpl {
                                    uopool_grpc_client: uopool_grpc_client.clone(),
                                }
                                .into_rpc(),
                            )?;
                        }

                        if api.contains("debug") {
                            let bundler_grpc_client =
                                BundlerClient::connect(format!("http://{}", "127.0.0.1:3002",))
                                    .await?;
                            server.add_method(
                                DebugApiServerImpl {
                                    uopool_grpc_client,
                                    bundler_grpc_client,
                                }
                                .into_rpc(),
                            )?;
                        }

                        let _handle = server.start().await?;
                        info!("Started bundler JSON-RPC server at {:}", "127.0.0.1:3000",);

                        pending::<Result<()>>().await
                    }
                });

                pending::<Result<()>>().await
            };
            rt.block_on(run_until_ctrl_c(task))?;
            Ok(())
        })?
        .join()
        .unwrap_or_else(|e| panic::resume_unwind(e))
}
