use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;

use rayon::iter::{IntoParallelIterator, ParallelBridge, ParallelIterator};
use serde::{Deserialize, Serialize};
use twox_hash::{XxHash3_64, XxHash3_128};
use walkdir::WalkDir;


const MAX_THREADS: usize = 64;
const CHUNK_SIZE: usize = 1_000_000;

struct ChunkedFile {
    file: std::fs::File,
}

impl ChunkedFile {
    fn new(f: std::fs::File) -> Self {
        ChunkedFile { file: f }
    }
}

impl Iterator for ChunkedFile {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut buffer = vec![0; CHUNK_SIZE];
        let read_count = self.file.read(&mut *buffer).unwrap();
        if read_count == 0 {
            return None;
        } else if read_count < buffer.len() {
            buffer.truncate(read_count);
        }
        Some(buffer)
    }
}

#[derive(Debug)]
pub enum LargeFileDigestResult<T> {
    Final(T),
    NotFinal(Vec<T>),
}

#[derive(Clone, Default, Deserialize, Serialize, clap::ValueEnum)]
#[clap(rename_all = "kebab-case")]
#[serde(rename_all = "snake_case")]
pub enum HashType {
    XxHash3_64,
    #[default]
    XxHash3_128,
}

#[derive(Clone, Default, Deserialize, Serialize, clap::ValueEnum)]
#[clap(rename_all = "kebab-case")]
#[serde(rename_all = "snake_case")]
pub enum FilepathSensitivity {
    // Warning: the `None` case has a special property. If you have a directory where any file (or file within any
    // subfolder) has a corresponding file with the same content, then the XOR operation used will result in a hash
    // value of zero. This is avoided if we are sensitive to the file path, as no two files can have the same path.
    // Consider using wrapping_add instead of XOR. Essentially, the binary operation should be both *comutative*
    // and *associative* in order for the result of 3 or more hashes combined to be permutation invariant.
    None,
    #[default]
    AsIs,
    Unicode,
    UnicodeLowercase,
}

fn zero_checksum(hash_type: &HashType) -> Vec<u8> {
    checksum(&vec![], hash_type)
}

pub fn directory_recurse_checksum(
    dirpath: &PathBuf,
    hash_type: &HashType,
    filepath_sensitive: &FilepathSensitivity,
) -> Vec<u8> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(MAX_THREADS)
        .build()
        .unwrap();

    let entries = WalkDir::new(&dirpath)
        .into_iter()
        .par_bridge()
        .map(|e| e.unwrap())
        .filter(|e| e.metadata().unwrap().is_file());
    let result = pool.install(|| {
        let (sender, receiver) = channel();
        entries
            .into_par_iter()
            .for_each_with(sender, |s, e| {
                s.send(file_checksum(&e.path(), hash_type, filepath_sensitive)).unwrap();
            });
        receiver
            .iter()
            .fold(zero_checksum(hash_type), |acc, x| {
                acc
                    .iter()
                    .zip(x)
                    .map(|(x1, x2)| x1 ^ x2)
                    .collect()
            })
    });
    result
}

pub fn file_checksum(fpath: &Path, hash_type: &HashType, filepath_sensitive: &FilepathSensitivity) -> Vec<u8> {
    let content_checksum = file_content_checksum(fpath, hash_type);

    match filepath_sensitive {
        FilepathSensitivity::AsIs | FilepathSensitivity::Unicode | FilepathSensitivity::UnicodeLowercase => {
            let fpath_checksums: Vec<_> = fpath
                .components()
                .filter(|p| {
                    match p {
                        std::path::Component::CurDir => false,
                        std::path::Component::ParentDir => unimplemented!(),
                        _ => true,
                    }
                })
                .map(|p| {
                    let fname_bytes = p.as_os_str().to_os_string();
                    let fname_bytes = match filepath_sensitive {
                        FilepathSensitivity::AsIs => fname_bytes.into_encoded_bytes(),
                        FilepathSensitivity::Unicode => fname_bytes
                            .into_string()
                            .expect("Failed to convert to unicode string.")
                            .into(),
                        FilepathSensitivity::UnicodeLowercase => fname_bytes
                            .into_string()
                            .expect("Failed to convert to unicode string.")
                            .to_lowercase()
                            .into(),
                        _ => unreachable!(),
                    };
                    tree_hash(fname_bytes, hash_type)
                })
                .collect();
            let fname_checksum = tree_hash_chunks(fpath_checksums.into_iter(), hash_type);
            tree_hash_chunks(vec![fname_checksum, content_checksum].into_iter(), hash_type)
        }
        FilepathSensitivity::None => content_checksum,
    }
}

pub fn file_content_checksum(fpath: &Path, hash_type: &HashType) -> Vec<u8> {
    let file = std::fs::File::open(fpath).unwrap();
    tree_hash_chunks(ChunkedFile::new(file), hash_type)
}

pub fn tree_hash(content: Vec<u8>, hash_type: &HashType) -> Vec<u8> {
    tree_hash_chunks(content.chunks(CHUNK_SIZE).map(|e| e.to_vec()), hash_type)
}

pub fn tree_hash_chunks<T>(content: T, hash_type: &HashType) -> Vec<u8>
where
    T: Iterator<Item = Vec<u8>> + Send,
{
    // This first separate processing is separate from the loop in case `content` has a different datatype than
    // `input_content`.
    let mut input_content = match large_content_block_digest(content, hash_type) {
        LargeFileDigestResult::NotFinal(result) => { result.into_iter() },
        LargeFileDigestResult::Final(result) => { return result; },
    };
    loop {
        input_content = match large_content_block_digest(input_content, hash_type) {
            LargeFileDigestResult::NotFinal(result) => { result.into_iter() },
            LargeFileDigestResult::Final(result) => { return result; },
        }
    }
}

fn checksum(content: &Vec<u8>, hash_type: &HashType) -> Vec<u8> {
    match hash_type {
        &HashType::XxHash3_64 => XxHash3_64::oneshot(&content).to_le_bytes().into(),
        &HashType::XxHash3_128 => XxHash3_128::oneshot(&content).to_le_bytes().into(),
    }
}

pub fn large_content_block_digest<T>(mut iter: T, hash_type: &HashType) -> LargeFileDigestResult<Vec<u8>>
where
    T: Iterator<Item = Vec<u8>> + Send,
{
    let mut block_checksums = Vec::<Vec<u8>>::new();
    let mut chunk;
    if let Some(c) = iter.next() {
        chunk = c;
    } else {
        return LargeFileDigestResult::Final(zero_checksum(hash_type));
    }
    loop {
        let (chunk_candidate, res) = rayon::join(
            || iter.next(),
            || checksum(&chunk, hash_type),
        );
        block_checksums.push(res);
        if let Some(c) = chunk_candidate {
            chunk = c;
        } else {
            break;
        }
    }
    if 1 == block_checksums.len() {
        let [single_checksum] = block_checksums.try_into().unwrap();
        LargeFileDigestResult::Final(single_checksum)
    } else if 1 < block_checksums.len() {
         LargeFileDigestResult::NotFinal(
            block_checksums
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .chunks(CHUNK_SIZE)
                .map(|e| e.into())
                .collect(),
        )
    } else {
        panic!();
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_content() {
        // % xxh3sum =(printf '')
        // XXH3_2d06800538d394c2  /tmp/zshSnJatY
        let mut expected = vec![0x2d, 0x06, 0x80, 0x05, 0x38, 0xd3, 0x94, 0xc2];
        expected.reverse();
        if let LargeFileDigestResult::Final(actual) = large_content_block_digest(
            vec![].into_iter(),
            &HashType::XxHash3_64,
        ) {
            assert_eq!(actual, expected);
        } else {
            panic!();
        }
    }


    fn test_content_checksum(content: Vec<Vec<u8>>, expected: Vec<u8>) {
        let mut expected = expected.clone();
        expected.reverse();
        if let LargeFileDigestResult::Final(actual) = large_content_block_digest(
            content.into_iter(),
            &HashType::XxHash3_64,
        ) {
            assert_eq!(actual, expected);
        } else {
            panic!();
        }
    }

    #[test]
    fn single_char_1() {
        // % xxh3sum =(printf '1')
        // XXH3_65cd25028f98f158  /tmp/zshGD2gqk
        // % xxd =(printf '1')
        // 00000000: 31                                       1
        test_content_checksum(vec![vec![0x31]], vec![0x65, 0xcd, 0x25, 0x02, 0x8f, 0x98, 0xf1, 0x58]);
    }

    #[test]
    fn single_char_newline() {
        // % xxh3sum =(printf '\n')
        // XXH3_384868fba0c21fdc  /tmp/zshbUOwbb
        // % xxd =(printf '\n')
        // 00000000: 0a                                       .
        test_content_checksum(vec![vec![0x0a]], vec![0x38, 0x48, 0x68, 0xfb, 0xa0, 0xc2, 0x1f, 0xdc]);
    }

    #[test]
    fn single_char_non_ascii() {
        // % xxh3sum =(echo 'fe' | xxd -r -p)
        // XXH3_6c197e51b9364ce3  /tmp/zsh8WgEPU
        // % echo 'fe' | xxd -r -p | xxd
        // 00000000: fe                                       .
        test_content_checksum(vec![vec![0xfe]], vec![0x6c, 0x19, 0x7e, 0x51, 0xb9, 0x36, 0x4c, 0xe3]);

        // % xxh3sum =(echo '2a' | xxd -r -p)
        // XXH3_79d03016b7aeed0d  /tmp/zshsmPs2Q
        test_content_checksum(vec![vec![0x2a]], vec![0x79, 0xd0, 0x30, 0x16, 0xb7, 0xae, 0xed, 0x0d]);
    }

    #[test]
    fn few_chars() {
        // % xxh3sum =(printf 'Hello!\nHi!')
        // XXH3_ebfbd3ac8d409913  /tmp/zsh36G8PR
        // % xxd =(printf 'Hello!\nHi!')
        // 00000000: 4865 6c6c 6f21 0a48 6921                 Hello!.Hi!
        test_content_checksum(
            vec![vec![0x48, 0x65, 0x6c, 0x6c, 0x6f, 0x21, 0x0a, 0x48, 0x69, 0x21]],
            vec![0xeb, 0xfb, 0xd3, 0xac, 0x8d, 0x40, 0x99, 0x13],
        )
    }
}
