use ethers::{
    prelude::{MiddlewareBuilder, NonceManagerMiddleware, SignerMiddleware},
    providers::{Http, Middleware, Provider},
    signers::{coins_bip39::English, LocalWallet, MnemonicBuilder, Signer},
    types::{TransactionRequest, U256},
    utils::{Geth, GethInstance},
};
use std::{ops::Mul, time::Duration};
use tempdir::TempDir;

type ClientType = NonceManagerMiddleware<SignerMiddleware<Provider<Http>, LocalWallet>>;
const KEY_PHRASE: &str = "test test test test test test test test test test test junk";

pub(crate) async fn setup_geth() -> anyhow::Result<(GethInstance, ClientType)> {
    let chain_id: u64 = 1337;
    let tmp_dir = TempDir::new("test_geth")?;

    let wallet = MnemonicBuilder::<English>::default()
        .phrase(KEY_PHRASE)
        .build()?;

    let geth = Geth::new().data_dir(tmp_dir.path().to_path_buf()).spawn();
    let provider =
        Provider::<Http>::try_from(geth.endpoint())?.interval(Duration::from_millis(10u64));

    let client = SignerMiddleware::new(provider.clone(), wallet.clone().with_chain_id(chain_id))
        .nonce_manager(wallet.address());

    let coinbase = client.get_accounts().await?[0];
    let tx = TransactionRequest::new()
        .to(wallet.address())
        .value(U256::from(10).pow(U256::from(18)).mul(100))
        .from(coinbase);
    provider.send_transaction(tx, None).await?.await?;

    Ok((geth, client))
}
