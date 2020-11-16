use serde::{de, Serialize};
use std::fmt::{self, Formatter};
use std::{sync::mpsc, thread, time};

use bitcoincore_rpc::json::{self, ImportMultiRescanSince, ScanningDetails};
use bitcoincore_rpc::{Client, Result as RpcResult, RpcApi};

const WAIT_INTERVAL: time::Duration = time::Duration::from_secs(7);

// Extensions for rust-bitcoincore-rpc

pub trait RpcApiExt: RpcApi {
    // Only supports the fields we're interested in (so not currently upstremable)
    fn get_block_stats(&self, blockhash: &bitcoin::BlockHash) -> RpcResult<GetBlockStatsResult> {
        let fields = (
            "height",
            "time",
            "total_size",
            "total_weight",
            "txs",
            "totalfee",
            "avgfeerate",
            "feerate_percentiles",
        );
        self.call("getblockstats", &[json!(blockhash), json!(fields)])
    }

    // Only supports the fields we're interested in (so not currently upstremable)
    fn get_mempool_info(&self) -> RpcResult<GetMempoolInfoResult> {
        self.call("getmempoolinfo", &[])
    }

    fn wait_blockchain_sync(
        &self,
        progress_tx: Option<mpsc::Sender<Progress>>,
    ) -> RpcResult<json::GetBlockchainInfoResult> {
        Ok(loop {
            let info = self.get_blockchain_info()?;

            if info.blocks == info.headers
                && (!info.initial_block_download || info.chain == "regtest")
            {
                break info;
            }

            info!(target: "bwt",
                "waiting for bitcoind to sync [{}/{} blocks, progress={:.1}%]",
                info.blocks, info.headers, info.verification_progress * 100.0
            );

            if let Some(ref progress_tx) = progress_tx {
                let progress = Progress::Sync {
                    progress_n: info.verification_progress as f32,
                    tip: info.median_time,
                };
                if progress_tx.send(progress).is_err() {
                    break info;
                }
            }
            thread::sleep(WAIT_INTERVAL);
        })
    }

    fn wait_wallet_scan(
        &self,
        progress_tx: Option<mpsc::Sender<Progress>>,
    ) -> RpcResult<json::GetWalletInfoResult> {
        Ok(loop {
            let info = self.get_wallet_info()?;
            match info.scanning {
                None => {
                    warn!("Your bitcoin node does not report the `scanning` status in `getwalletinfo`. It is recommended to upgrade to Bitcoin Core v0.19+ to enable this.");
                    warn!("This is needed for bwt to wait for scanning to finish before starting up. Starting bwt while the node is scanning may lead to unexpected results. Continuing anyway...");
                    break info;
                }
                Some(ScanningDetails::NotScanning(_)) => break info,
                Some(ScanningDetails::Scanning { progress, duration }) => {
                    let duration = duration as u64;
                    let progress_n = progress as f32;
                    let eta = (duration as f32 / progress_n) as u64 - duration;

                    info!(target: "bwt",
                        "waiting for bitcoind to finish scanning [done {:.1}%, running for {}m, eta {}m]",
                        progress_n * 100.0, duration / 60, eta / 60
                    );

                    if let Some(ref progress_tx) = progress_tx {
                        let progress = Progress::Scan { progress_n, eta };
                        if progress_tx.send(progress).is_err() {
                            break info;
                        }
                    }
                }
            };
            thread::sleep(WAIT_INTERVAL);
        })
    }
}

impl RpcApiExt for Client {}

#[derive(Debug, Copy, Clone)]
pub enum Progress {
    Sync { progress_n: f32, tip: u64 },
    Scan { progress_n: f32, eta: u64 },
}

#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub struct GetBlockStatsResult {
    pub height: u64,
    pub time: u64,
    pub txs: u64,
    pub total_weight: u64,
    pub total_size: u64,
    #[serde(rename = "totalfee", with = "bitcoin::util::amount::serde::as_sat")]
    pub total_fee: bitcoin::Amount,
    #[serde(rename = "avgfeerate")]
    pub avg_fee_rate: u64,
    pub feerate_percentiles: (u64, u64, u64, u64, u64),
}

#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub struct GetMempoolInfoResult {
    pub size: u64,
    pub bytes: u64,
    #[serde(
        rename = "mempoolminfee",
        with = "bitcoin::util::amount::serde::as_btc"
    )]
    pub mempool_min_fee: bitcoin::Amount,
}

// Wrap rust-bitcoincore-rpc's RescanSince to enable deserialization
// Pending https://github.com/rust-bitcoin/rust-bitcoincore-rpc/pull/150

#[derive(Clone, PartialEq, Eq, Copy, Debug, Serialize)]
pub enum RescanSince {
    Now,
    Timestamp(u64),
}

impl Into<ImportMultiRescanSince> for &RescanSince {
    fn into(self) -> ImportMultiRescanSince {
        match self {
            RescanSince::Now => ImportMultiRescanSince::Now,
            RescanSince::Timestamp(t) => ImportMultiRescanSince::Timestamp(*t),
        }
    }
}

impl<'de> serde::Deserialize<'de> for RescanSince {
    fn deserialize<D>(deserializer: D) -> Result<RescanSince, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = RescanSince;

            fn expecting(&self, formatter: &mut Formatter) -> fmt::Result {
                write!(formatter, "unix timestamp or 'now'")
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(RescanSince::Timestamp(value))
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value == "now" {
                    Ok(RescanSince::Now)
                } else {
                    Err(de::Error::custom(format!(
                        "invalid str '{}', expecting 'now' or unix timestamp",
                        value
                    )))
                }
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}
