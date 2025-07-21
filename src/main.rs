use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::Parser;
use clap::{crate_version, crate_name};
use serde::{Deserialize, Serialize};
use serde_json;

use rchecksum::HashType;


#[derive(Parser)]
#[command(about, version)]
struct Args {
    #[arg(long = "base-algo", default_value_t, value_enum)]
    base_hash_algo: HashType,

    #[arg(
        required = true,
        help = "One checksum will be computed for each given path. Each path may refer to a directory or a regular \
                file.",
    )]
    paths: Vec<PathBuf>,
}

#[derive(Deserialize, Serialize)]
struct FinalResult {
    hashes: BTreeMap<PathBuf, SingleChecksumResult>,
}

#[derive(Deserialize, Serialize)]
struct SingleChecksumResult {
    #[serde(rename = "type")]
    type_: String,
    base_algo: HashType,
    version: String,
    hash: String,
}

impl FinalResult {
    fn new() -> Self {
        Self { hashes: BTreeMap::new() }
    }
}

fn main() {
    let args = Args::parse();

    let mut final_result = FinalResult::new();

    for path in args.paths {
        let checksum = rchecksum::directory_recurse_checksum(&path, &args.base_hash_algo);
        let hash_string: String = checksum.into_iter().rev().map(|b| format!("{b:x}")).collect::<Vec<_>>().join("");
        final_result.hashes.insert(path, SingleChecksumResult {
            type_: crate_name!().to_string(), base_algo: args.base_hash_algo.clone(),
            version: crate_version!().to_string(), hash: hash_string,
        });
    }
    let result_formatted = serde_json::to_string_pretty(&final_result)
        .expect("Failed to convert result to JSON string.");
    println!("{}", result_formatted);
}
