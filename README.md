# Planned/potential features

- [ ] Checking file integrity given output `.json` file, similarly to `shasum --check`.
- [ ] Specifying multiple base algorithms or filepath-sensitivity modes so that multiple hashes for a same file/directory are computed in a single pass. E.g., `rchecksum --fpath-mode as-is --fpath-mode none --fpath-mode unicode --base-algo xx-hash3-128 --base-algo xx-hash3-64`.
