
[<img alt="crates.io" src="https://img.shields.io/crates/v/avalanche-types.svg?style=for-the-badge&color=fc8d62&logo=rust" height="20">](https://crates.io/crates/avalanche-types)
[<img alt="docs.rs" src="https://img.shields.io/badge/docs.rs-avalanche_types-66c2a5?style=for-the-badge&labelColor=555555&logo=docs.rs" height="20">](https://docs.rs/avalanche-types)
![Github Actions](https://github.com/ava-labs/avalanche-types-rs/actions/workflows/test-and-release.yml/badge.svg)

`avalanche-types` crate implements Avalanche primitive types in Rust:
- Ids (e.g., [`src/ids`](./src/ids))
- Transaction types/serialization (e.g., [`src/platformvm/txs`](./src/platformvm/txs))
- Certificates (e.g., [`src/key/cert`](./src/key/cert))
- Keys and addresses (e.g., [`src/key/secp256k1`](./src/key/secp256k1))
- Peer-to-peer messages (e.g., [`src/message`](./src/message))
- RPC chain VM (e.g., [`src/subnet/rpc`](./src/subnet/rpc))
- Genesis generate helper (e.g., [`src/subnet_evm`](./src/subnet_evm))

The basic types available in this crate are used in other Avalanche Rust projects (e.g., distributed load tester [`blizzard`](https://talks.gyuho.dev/distributed-load-generator-avalanche-2022.html), [`avalanche-ops`](https://github.com/ava-labs/avalanche-ops)).
