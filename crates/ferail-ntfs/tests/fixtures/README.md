# Parser fixtures

The byte-backed fixtures for the neutral parser are assembled in
`src/record.rs` by `RecordFixture`. Keeping the field writes next to the
assertions makes every deliberately corrupt offset visible in review and
avoids committing a personal or machine-derived MFT image.

The suite covers a valid resident record, raw ill-formed UTF-16, USA failure,
truncated attributes, fragmented/sparse mapping pairs, negative LCN deltas,
an attribute-list extension reference and DOS-alias filtering. Windows VHDX
creation scripts belong in the later raw-reader qualification step; raw volume
bytes must never be checked into this repository.
