use serde::{self, Deserialize, Deserializer, Serializer};
use serde_with::{DeserializeAs, SerializeAs};

pub fn serialize<S>(x: &crate::txs::utxo::Utxo, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let utxo_hex = x.to_hex().map_err(serde::ser::Error::custom)?;
    serializer.serialize_str(&utxo_hex)
}

pub fn deserialize<'de, D>(deserializer: D) -> Result<crate::txs::utxo::Utxo, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;

    crate::txs::utxo::Utxo::from_hex(&s).map_err(serde::de::Error::custom)
}

pub struct Hex0xUtxo(crate::txs::utxo::Utxo);

impl SerializeAs<crate::txs::utxo::Utxo> for Hex0xUtxo {
    fn serialize_as<S>(x: &crate::txs::utxo::Utxo, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let s = x.to_hex().map_err(serde::ser::Error::custom)?;

        serializer.serialize_str(&s)
    }
}

impl<'de> DeserializeAs<'de, crate::txs::utxo::Utxo> for Hex0xUtxo {
    fn deserialize_as<D>(deserializer: D) -> Result<crate::txs::utxo::Utxo, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;

        crate::txs::utxo::Utxo::from_hex(&s).map_err(serde::de::Error::custom)
    }
}
