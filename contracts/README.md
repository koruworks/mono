# Koru Contract

`koru.v1` is Koru's language-neutral domain projection and command contract.
It is not the persisted document and it is not a CRDT protocol. Future Store
adapters project Loro state into these messages and translate semantic commands
back into model operations; Loro synchronization remains standard Loro Protocol.

Run all commands from the repository root:

```sh
buf format -w contracts
buf lint contracts
buf breaking contracts --against '.git#branch=main,subdir=contracts'
buf generate contracts --template contracts/buf.gen.yaml
```

Generated DTOs are adapter-boundary values. SwiftUI and Compose state must use
native domain types. Add fields in place, reserve deleted numbers and names, and
create `koru.v2` only for a genuinely incompatible semantic change.

Block payloads use `google.protobuf.Any` deliberately. An unsupported block is
rendered as a placeholder while its type URL and bytes remain untouched. Mobile
clients send commands rather than writing complete projections, so an older
client cannot erase state it does not understand.

## Wire-contract vectors

Blob receivers support [verified empty files](vectors/cloud-blob-object-v1/EMPTY-FILES.md)
through the ordinary durable publication path.

`vectors/` holds the checked-in test vectors of the Cloud wire contracts
(`cloud-blob-object-v1`, `host-control-v1`, `cloud-sync-admission-v1`,
`workspace-crypto` and the rest). They are read by the test suites of the
public crates that speak those contracts, through `include_str!` and paths
relative to this directory, so **their bytes are the contract**: a vector
changes only with the contract it encodes, and moving one breaks the build.
Each folder's `README.md` says what its columns mean.

The transfer carrier's [existing UUIDv5 document compatibility](vectors/cloud-transfer-v1/DOCUMENT-IDS.md)
extends document-ID acceptance without rewriting stored identities; deploy
support to Cloud workers before clients.
