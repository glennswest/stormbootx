# Test fixtures

- `stormcentral-rustnic-serial1.manifest`, `.sig`: stormcentral's first
  promotion of `stormbootx-rustnic` (serial 1, v0.14.0), exactly as
  `GET /api/v1/boothelpers/stormbootx-rustnic/current` and `current.sig`
  served them on 2026-10-06. The signature is stormcentral's own release key
  (`manifest::RELEASE_KEYS`), hex as stormcentral serves it.
  `tests/update-boots.sh` serves them to stormbootx (#89) to show the binary
  accepts a real signature, not only the test key's. Public data: a signed
  list of file digests. Do not edit them; a changed byte fails the check.
