# [Royal Road](https://royalroad.com/) fiction downloader
CLI tool for downloading fictions from [Royal Road](https://royalroad.com/) incrementally and periodically. Should be able to run the CLI with `--incremental` in a scheduled job to keep the downloaded copy up-to-date.
# Goals
- [x] Incremental downloads work even if fiction changes metadata (fiction title, chapter title, etc.).
- [x] Download new chapters while preserving old chapters even when old chapters are publicly removed (STUB).
# Usage
```txt
Incremental periodic downloader for RoyalRoad.

Usage: royalroad-dl.exe [-p=PATH] [-t=MS] [-c=NUM] [-i] URL

Available positional items:
    URL                    The main page (e.g. table of contents) of the content to download.

Available options:
    -p, --path=PATH        Custom output path.
    -t, --time-limit=MS    Minimum ms per request. Can't be zero.
                           [default: 1500]
    -c, --connections=NUM  Concurrent connections limit. Zero indicates no limit.
                           [default: 4]
    -i, --incremental      Incremental download. Auto-detect previously downloaded and only download
                           new.
    -h, --help             Prints help information
    -V, --version          Prints version information
```

# Installing
Download the correct release for your platform from [releases](https://github.com/Easyoakland/royalroad-dl/releases)
# Building from source
- Install rust compiler and tool-chain <https://www.rust-lang.org/tools/install>
- Compile with `cargo build --release` and find the output binary in the `target/release` directory
# Issues
If Royal Road changes its website layout this downloader may stop working. If this occurs please file an [issue](https://github.com/Easyoakland/royalroad-dl/issues).
