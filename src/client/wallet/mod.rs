pub mod p;
pub mod x;

#[cfg(feature = "evm")]
pub mod evm;

use std::{
    fmt,
    io::{self, Error, ErrorKind},
    sync::{Arc, Mutex},
};

use crate::{
    client::{evm as api_evm, info as api_info, x as api_x},
    ids::{self, short},
    key, units,
};

#[derive(Debug, Clone)]
pub struct Wallet<T: key::secp256k1::ReadOnly + key::secp256k1::SignOnly + Clone> {
    pub keychain: key::secp256k1::keychain::Keychain<T>,

    pub http_rpcs: Vec<String>,
    pub http_rpc_cursor: Arc<Mutex<usize>>, // to roundrobin

    pub network_id: u32,
    pub network_name: String,

    pub h160_address: primitive_types::H160,
    pub x_address: String,
    pub p_address: String,
    pub c_address: String,
    pub short_address: short::Id,
    pub eth_address: String,

    pub blockchain_id_x: ids::Id,
    pub blockchain_id_p: ids::Id,
    pub blockchain_id_c: ids::Id,

    pub chain_id_c: primitive_types::U256,

    pub avax_asset_id: ids::Id,

    /// Fee that is burned by every non-state creating transaction.
    pub tx_fee: u64,
    /// Transaction fee for adding a primary network validator.
    pub add_primary_network_validator_fee: u64,
    /// Transaction fee to create a new subnet.
    pub create_subnet_tx_fee: u64,
    /// Transaction fee to create a new blockchain.
    pub create_blockchain_tx_fee: u64,
}

/// ref. https://doc.rust-lang.org/std/string/trait.ToString.html
/// ref. https://doc.rust-lang.org/std/fmt/trait.Display.html
/// Use "Self.to_string()" to directly invoke this
impl<T> fmt::Display for Wallet<T>
where
    T: key::secp256k1::ReadOnly + key::secp256k1::SignOnly + Clone,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "http_rpcs: {:?}\n", self.http_rpcs)?;
        write!(f, "network_id: {}\n", self.network_id)?;
        write!(f, "network_name: {}\n", self.network_name)?;

        write!(f, "h160_address: {}\n", self.h160_address)?;
        write!(f, "x_address: {}\n", self.x_address)?;
        write!(f, "p_address: {}\n", self.p_address)?;
        write!(f, "c_address: {}\n", self.c_address)?;
        write!(f, "short_address: {}\n", self.short_address)?;
        write!(f, "eth_address: {}\n", self.eth_address)?;

        write!(f, "blockchain_id_x: {}\n", self.blockchain_id_x)?;
        write!(f, "blockchain_id_p: {}\n", self.blockchain_id_p)?;
        write!(f, "blockchain_id_c: {}\n", self.blockchain_id_c)?;

        write!(f, "chain_id_c: {}\n", self.chain_id_c)?;

        write!(f, "avax_asset_id: {}\n", self.avax_asset_id)?;

        write!(f, "tx_fee: {}\n", self.tx_fee)?;
        write!(
            f,
            "add_primary_network_validator_fee: {}\n",
            self.add_primary_network_validator_fee
        )?;
        write!(f, "create_subnet_tx_fee: {}\n", self.create_subnet_tx_fee)?;
        write!(
            f,
            "create_blockchain_tx_fee: {}\n",
            self.create_blockchain_tx_fee
        )
    }
}

impl<T> Wallet<T>
where
    T: key::secp256k1::ReadOnly + key::secp256k1::SignOnly + Clone,
{
    /// Picks one endpoint in roundrobin, and updates the cursor for next calls.
    /// Returns the pair of an index and its corresponding endpoint.
    pub fn pick_http_rpc(&self) -> (usize, String) {
        let mut idx = self.http_rpc_cursor.lock().unwrap();

        let picked = *idx;
        let http_rpc = self.http_rpcs[picked].clone();
        *idx = (picked + 1) % self.http_rpcs.len();

        log::debug!("picked http rpc {} at index {}", http_rpc, picked);
        (picked, http_rpc)
    }

    #[must_use]
    pub fn x(&self) -> x::X<T> {
        x::X {
            inner: self.clone(),
        }
    }

    #[must_use]
    pub fn p(&self) -> p::P<T> {
        p::P {
            inner: self.clone(),
        }
    }

    /// Set "chain_id_alias" to either "C" or subnet_evm chain Id.
    /// e.g., "/ext/bc/C/rpc"
    #[cfg(feature = "evm")]
    #[must_use]
    pub fn evm<'a, S>(
        &self,
        eth_signer: &'a S,
        chain_id_alias: String,
        chain_id: primitive_types::U256,
    ) -> io::Result<evm::Evm<'a, T, S>>
    where
        S: ethers_signers::Signer + Clone,
        S::Error: 'static,
    {
        let chain_rpc_url_path = format!("/ext/bc/{}/rpc", chain_id_alias).to_string();
        let mut providers = Vec::new();
        for http_rpc in self.http_rpcs.iter() {
            let provider = ethers_providers::Provider::<ethers_providers::Http>::try_from(
                format!("{http_rpc}{chain_rpc_url_path}").as_str(),
            )
            .map_err(|e| {
                Error::new(
                    ErrorKind::Other,
                    format!("failed to create provider '{}'", e),
                )
            })?;
            providers.push(provider);
        }
        Ok(evm::Evm::<'a, T, S> {
            inner: self.clone(),
            eth_signer,
            providers,
            chain_id,
            chain_id_alias,
            chain_rpc_url_path,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Builder<T: key::secp256k1::ReadOnly + key::secp256k1::SignOnly + Clone> {
    pub key: T,
    pub http_rpcs: Vec<String>,
}

impl<T> Builder<T>
where
    T: key::secp256k1::ReadOnly + key::secp256k1::SignOnly + Clone,
{
    pub fn new(key: &T) -> Self {
        Self {
            http_rpcs: Vec::new(),
            key: key.clone(),
        }
    }

    /// Adds an HTTP rpc endpoint to the `http_rpcs` field in the Builder.
    #[must_use]
    pub fn http_rpc(mut self, http_rpc: String) -> Self {
        if self.http_rpcs.is_empty() {
            self.http_rpcs = vec![http_rpc];
        } else {
            self.http_rpcs.push(http_rpc);
        }
        self
    }

    /// Overwrites the HTTP rpc endpoints to the `http_rpcs` field in the Builder.
    #[must_use]
    pub fn http_rpcs(mut self, http_rpcs: Vec<String>) -> Self {
        self.http_rpcs = http_rpcs;
        self
    }

    pub async fn build(&self) -> io::Result<Wallet<T>> {
        log::info!("building wallet with {} endpoints", self.http_rpcs.len());

        let keychain = key::secp256k1::keychain::Keychain::new(vec![self.key.clone()]);
        let h160_address = keychain.keys[0].h160_address();

        let resp = api_info::get_network_id(&self.http_rpcs[0]).await?;
        let network_id = resp.result.unwrap().network_id;
        let resp = api_info::get_network_name(&self.http_rpcs[0]).await?;
        let network_name = resp.result.unwrap().network_name;

        let resp = api_info::get_blockchain_id(&self.http_rpcs[0], "X").await?;
        let blockchain_id_x = resp.result.unwrap().blockchain_id;

        let resp = api_info::get_blockchain_id(&self.http_rpcs[0], "P").await?;
        let blockchain_id_p = resp.result.unwrap().blockchain_id;

        let resp = api_info::get_blockchain_id(&self.http_rpcs[0], "C").await?;
        let blockchain_id_c = resp.result.unwrap().blockchain_id;

        let resp = api_evm::chain_id(&self.http_rpcs[0], "C")
            .await
            .map_err(|e| {
                Error::new(
                    ErrorKind::Other,
                    format!("failed to get chainId for C-chain '{}'", e),
                )
            })?;
        let chain_id_c = resp.result;

        let resp = api_x::get_asset_description(&self.http_rpcs[0], "AVAX").await?;
        let resp = resp
            .result
            .expect("unexpected None GetAssetDescriptionResult");
        let avax_asset_id = resp.asset_id;

        let resp = api_info::get_tx_fee(&self.http_rpcs[0]).await?;
        let tx_fee = resp.result.unwrap().tx_fee;

        let (create_subnet_tx_fee, create_blockchain_tx_fee) = if network_id == 1 {
            // ref. "genesi/genesis_mainnet.go"
            (1 * units::AVAX, 1 * units::AVAX)
        } else {
            // ref. "genesi/genesis_fuji.go"
            // ref. "genesi/genesis_local.go"
            (100 * units::MILLI_AVAX, 100 * units::MILLI_AVAX)
        };

        let w = Wallet {
            keychain,

            http_rpcs: self.http_rpcs.clone(),
            http_rpc_cursor: Arc::new(Mutex::new(0)),

            network_id,
            network_name,

            h160_address,
            x_address: self.key.hrp_address(network_id, "X").unwrap(),
            p_address: self.key.hrp_address(network_id, "P").unwrap(),
            c_address: self.key.hrp_address(network_id, "C").unwrap(),
            short_address: self.key.short_address().unwrap(),
            eth_address: self.key.eth_address(),

            blockchain_id_x,
            blockchain_id_p,
            blockchain_id_c,

            chain_id_c,

            avax_asset_id,

            tx_fee,
            add_primary_network_validator_fee: ADD_PRIMARY_NETWORK_VALIDATOR_FEE,
            create_subnet_tx_fee,
            create_blockchain_tx_fee,
        };
        log::info!("initiated the wallet:\n{}", w);

        Ok(w)
    }
}

// ref. https://docs.avax.network/learn/platform-overview/transaction-fees/#fee-schedule
pub const ADD_PRIMARY_NETWORK_VALIDATOR_FEE: u64 = 0;
