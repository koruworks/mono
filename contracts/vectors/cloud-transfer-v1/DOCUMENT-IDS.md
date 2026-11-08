# Existing UUIDv5 document identities

The Cloud transfer carrier accepts canonical RFC-variant UUIDv4 and UUIDv5
`document_id` values. Existing deterministic document identities must survive
transfer with their original identity and links. Workspace and Profile authority
IDs remain UUIDv4, and scope, membership, epoch and claim checks remain required.

This is a receiver compatibility extension to the existing version-1 record;
there are no new fields, stored-ID rewrites, or format-version changes. Deploy
support to Cloud workers before clients begin sending UUIDv5 document transfers.
Older receivers continue to refuse such transfers with `sync_transfer_invalid`.

The regression `uuid_v5_documents_keep_their_identity_across_control_and_loro_metadata`
covers the control, object mapping, join and durable-ack paths and keeps foreign
scope, non-v4 authority IDs and invalid UUID variants refused.
