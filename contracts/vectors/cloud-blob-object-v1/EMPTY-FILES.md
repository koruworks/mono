# Empty files

Receivers accept length zero only for
`b3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262`,
the BLAKE3 address of the empty file. A zero size at any other address remains
invalid. The 2 GiB ceiling, authority checks and wire shape are unchanged.

The client allocates normally and sends no PATCH because no byte range exists.
Completion creates and syncs an empty staging file, then follows normal hash
verification, immutable publication and durable SQL reporting. A download still
requires an authorized durable read grant; it creates an empty staging file and
verifies its hash before installation. Coverage requires that durable mapping.

Deploy the server and its additive constraint migration before the client.
The migration preserves existing rows and allows zero only at this exact hash.
Older clients still refuse empty-file coverage; they must be updated to complete
workspaces which contain empty files.
