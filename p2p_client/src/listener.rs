//! listener.rs
//! by Lazuli Kleinhans, Liam Keane, Ruben Boero
//! May 29th, 2025
//! CS347 Advanced Software Design

use crate::encryption;
use crate::file_rw;
use crate::packet;
use aes_gcm::{
    Aes256Gcm, Key,
    aead::{KeyInit, OsRng},
};
use directories::ProjectDirs;
use hex;
use serde::{Deserialize, Serialize};
use serde_json;
use size::Size;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use tokio::runtime::Runtime;
use memmap2::Mmap;
use x25519_dalek::{EphemeralSecret, PublicKey};

type CatalogMap = HashMap<String, FileInfo>;

#[derive(Hash, Eq, PartialEq, Debug, Serialize, Deserialize)]
struct FileInfo {
    file_path: String,
    file_size: u64
}

impl FileInfo {
    fn new(file_path: String, file_size: u64) -> FileInfo {
        FileInfo {
            file_path: file_path,
            file_size: file_size
        }
    }
}



/// Gets the path to the catalog. If catalog doesn't exist, a new one is created.
/// The catalog is stored in a static directory.
///
/// The location of static directory depends on the OS:
///
/// Linux: `/home/[user]/.local/share/p2p_client`
/// macOS: `/Users/[user]/Library/Application Support/com.LLR.p2p_client`
/// Windows: `C:\Users\[user]\AppData\Roaming\LLR\p2p_client\data`
fn get_catalog_path() -> Result<PathBuf, String> {
    // find existing catalog or create a new one
    let mut catalog_path = match ProjectDirs::from("com", "LLR", "p2p_client") {
        Some(d) => d.data_dir().to_owned().to_path_buf(),
        None => return Err(format!("No valid config directory could be located"))
    };

    if let Err(e) = fs::create_dir_all(&catalog_path) {
        return Err(format!("Failed to create catalog directory: {e}"));
    }

    catalog_path.push("catalog.json");

    Ok(catalog_path)
}



/// Returns catalog as Hashmap given the absolute path to it.
/// If there is no catalog.json file, creates the file and returns an empty Hashmap
fn get_deserialized_catalog(catalog_path: &PathBuf) -> Result<CatalogMap, String> {
    let catalog: CatalogMap;

    if catalog_path.exists() {
        let serialized = match fs::read_to_string(&catalog_path) {
            Ok(c) => c,
            Err(e) => return Err(e.to_string())
        };

        let deserialized = match serde_json::from_str(&serialized) {
            Ok(d) => d,
            Err(e) => return Err(e.to_string())
        };

        catalog = deserialized;
    } else {
        // create the file if it doesn't exist
        let empty_catalog: CatalogMap = HashMap::new();
        write_updated_catalog(catalog_path, &empty_catalog)?;
        catalog = empty_catalog;
    }

    Ok(catalog)
}



/// Writes changes made to catalog. If there is not file at the given path, it will create a file and 
/// populate it with a bare json list: {}
fn write_updated_catalog(catalog_path: &PathBuf, catalog: &CatalogMap) -> Result<(), String> {
    // write updated catalog to catalog.json
    let mut json_file = match File::create(catalog_path) {
        Ok(f) => f,
        Err(e) => return Err(format!("Failed to open catalog file: {e}"))
    };

    let json = match serde_json::to_string_pretty(catalog) {
        Ok(j) => j,
        Err(e) => return Err(format!("Failed to serialize catalog: {e}"))
    };

    if let Err(e) = json_file.write_all(json.as_bytes()) {
        return Err(format!("Failed to write catalog file: {e}"));
    }

    Ok(())
}



/// Given a file path as input, computes hash of the file, then stores the hash and absolute file path in
/// catalog.json found in a static directory. See get_catalog_path() for catalog directory locations
pub fn add_file_to_catalog(file_path: &String) -> Result<(), String> {
    let catalog_path = match get_catalog_path() {
        Ok(p) => p,
        Err(e) => return Err(format!("Failed to retreive catalog path: {e}"))
    };

    let mut catalog = match get_deserialized_catalog(&catalog_path) {
        Ok(c) => c,
        Err(e) => return Err(format!("Failed to retreive catalog: {e}"))
    };

    let absolute_file_path = match fs::canonicalize(&file_path) {
        Ok(p) => p,
        Err(e) => return Err(format!("Unable to get the requested file's absolute path: {e}"))
    };

    // for large files (> ~100 MB) hashing can take a significant amount of time
    println!("Computing file hash...");

    // get hash of file
    let file_bytes = match file_rw::read_file_bytes(&absolute_file_path) {
        Ok(b) => b,
        Err(e) => return Err(e)
    };
    let file_hash = packet::compute_sha256_hash(&file_bytes);
    let file_hash_string: String = hex::encode(&file_hash);

    // get size of file
    let file_size  = match file_rw::get_file_size(&absolute_file_path) {
        Ok(s) => s,
        Err(e) => return Err(e)
    };

    // put file path and file size into a FileInfo object
    let file_info = FileInfo::new(absolute_file_path.to_string_lossy().into_owned(), file_size);

    // add/update entry in catalog
    catalog.insert(file_hash_string.clone(), file_info);

    if let Err(e) = write_updated_catalog(&catalog_path, &catalog) {
        return Err(format!("Error writing updated catalog: {}", e));
    }

    println!("Successfully added {file_path} ({file_hash_string}) to catalog");

    Ok(())
}



/// Given a file hash as input, removes the associated entry from the catalog
///
/// If the input hash is `DELETE-ALL` then all entries in the catalog will be removed
pub fn remove_file_from_catalog(hash: &String) -> Result<(), String> {
    let catalog_path = match get_catalog_path() {
        Ok(p) => p,
        Err(e) => return Err(format!("Failed to retreive catalog path: {e}"))
    };

    let mut catalog = match get_deserialized_catalog(&catalog_path) {
        Ok(c) => c,
        Err(e) => return Err(format!("Failed to retreive catalog: {e}"))
    };

    if hash == "DELETE-ALL" {
        catalog.clear();
        println!("Successfully removed all entries from catalog");
    } else {
        match catalog.remove(hash) {
            None => println!("Entry \"{hash}\" does not exist in catalog"),
            Some(f) => {
                let file_name = PathBuf::from(f.file_path).file_name().unwrap().to_string_lossy().into_owned();
                println!("Successfully removed {file_name} ({hash}) from catalog")
            }
        };
    }

    // write updated catalog to catalog.json
    if let Err(e) = write_updated_catalog(&catalog_path, &catalog) {
        return Err(format!("Error writing updated catalog: {}", e));
    }

    Ok(())
}



/// Displays the contents of the user's local catalog
pub fn view_catalog() -> Result<(), String> {
    let catalog_path = match get_catalog_path() {
        Ok(p) => p,
        Err(e) => return Err(format!("Failed to retrieve catalog path: {e}"))
    };

    let catalog = match get_deserialized_catalog(&catalog_path) {
        Ok(c) => c,
        Err(e) => return Err(format!("Failed to retrieve catalog: {e}"))
    };

    if catalog.is_empty() {
        println!("Catalog is empty.");
        return Ok(());
    }

    // dynamically determine max name length
    let max_name_len = catalog
        .values() // get iterator over the file paths stored in catalog
        // for each path, get the name of the file and its length
        .filter_map(|info| {
            let name = Path::new(&info.file_path).file_name()?.to_str()?;
            Some(name.len())
        })
        // make sure that we don't go under the length of the table header
        .filter(|length| length > &"File Name".len())
        .max()
        .unwrap_or("File Name".len());

    // dynamically determine max size length
    let max_size_len = catalog
        .values()
        .map(|info| Size::from_bytes(info.file_size).to_string().len())
        .filter(|length| length > &"Size".len())
        .max()
        .unwrap_or("Size".len());

    // sha256 hashes are 64 characters long
    let hash_len = 64;

    // print table header
    println!(
        "| {:<hash_len$} | {:<max_name_len$} | {:<max_size_len$}",
        "SHA-256 Hash", "File Name", "Size"
    );

    // 2 gives space for the bars separating columns
    println!(
        "|{}|{}|{}",
        "=".repeat(2 + hash_len),
        "=".repeat(2 + max_name_len),
        "=".repeat(2 + max_size_len)
    );

    // print each catalog entry
    for (hash, info) in catalog.iter() {
        let file_name = Path::new(&info.file_path)
            .file_name()
            .and_then(|os_str| os_str.to_str())
            .unwrap_or("invalid UTF-8");

        let file_size = Size::from_bytes(info.file_size).to_string();

        println!(
            "| {:<hash_len$} | {:<max_name_len$} | {:<max_size_len$}",
            hash, file_name, file_size
        );
    }

    Ok(())
}



/// Handles sending listener's catalog to requester
fn fulfill_catalog_request(
    stream: &mut TcpStream,
    nonce: &mut [u8; 12],
    cipher: &Aes256Gcm
) -> Result<(), String> {
    let catalog_path = match get_catalog_path() {
        Ok(p) => p,
        Err(e) => return Err(format!("Failed to retrieve catalog path: {e}"))
    };

    let catalog = match get_deserialized_catalog(&catalog_path) {
        Ok(c) => c,
        Err(e) => return Err(format!("Failed to retrieve catalog: {e}"))
    };

    // remove absolute paths from catalog before sending
    // important for security and privacy concerns to remove BEFORE sending packet
    let mut pathless_catalog = CatalogMap::new();
    if !catalog.is_empty() {
        // modify each catalog entry
        for (hash, info) in catalog.iter() {
            let file_name = Path::new(&info.file_path)
                .file_name()
                .and_then(|os_str| os_str.to_str())
                .unwrap_or("invalid UTF-8")
                .to_string();
            pathless_catalog.insert(hash.clone(), FileInfo::new(file_name, info.file_size));
        }
    }
    
    let catalog_bytes = match serde_json::to_string(&pathless_catalog) {
        Ok(j) => j.into_bytes(),
        Err(e) => return Err(format!("Failed to serialize catalog: {e}"))
    };

    let message = packet::encode_packet(catalog_bytes);
    if let Err(e) = encryption::send_to_connection(stream, nonce, cipher, message) {
        return Err(format!("Failed to send catalog: {e}"));
    }

    Ok(())
}



/// Send a file name and its size to the requesting TcpStream
fn send_file_metadata(
    file_path: &PathBuf,
    cipher: &Aes256Gcm,
    mut nonce: &mut [u8; 12],
    mut stream: &mut TcpStream
) -> Result<(), String> {
    // send file name
    match file_path.file_name() {
        Some(f) => {
            let file_name_packet = packet::encode_packet(f.to_string_lossy().into_owned().into_bytes());
            if let Err(e)  = encryption::send_to_connection(&mut stream, &mut nonce, &cipher, file_name_packet) {
                return Err(format!("Unable to send file name: {e}"));
            }
        },
        None => return Err(format!("Unable to get file name from file path"))
    }
    
    // send file size
    let file_size = match file_rw::get_file_size(&file_path) {
        Ok(s) => s,
        Err(e) => return Err(e)
    };
    let file_size_bytes = file_size.to_be_bytes().to_vec();
    let file_size_packet = packet::encode_packet(file_size_bytes);
    if let Err(e) = encryption::send_to_connection(&mut stream, &mut nonce, &cipher, file_size_packet) {
        return Err(format!("Unable to send file size: {e}"));
    }

    Ok(())
}



/// Returns the absolute file path of a file (from the catalog) given its hash
pub fn get_file_from_catalog(hash: &String) -> Result<PathBuf, String> {
    // load existing catalog or create a new one
    let catalog_path = match get_catalog_path() {
        Ok(p) => p,
        Err(e) => return Err(format!("Failed to retreive catalog path: {e}"))
    };

    let catalog = match get_deserialized_catalog(&catalog_path) {
        Ok(c) => c,
        Err(e) => return Err(format!("Failed to retreive catalog: {e}"))
    };

    // get file path from catalog
    let file_path = match catalog.get(hash) {
        Some(i) => i.file_path.to_owned(),
        None => return Err(format!("Requested file does not exist in catalog"))
    };

    Ok(PathBuf::from(file_path))
}



/// Handles sending requested file to requester
fn fulfill_file_request(
    mut stream: &mut TcpStream,
    mut nonce: &mut [u8; 12],
    cipher: &Aes256Gcm
) -> Result<(), String> {
    // listen for hash of file to send
    let mut buffer = [0u8; packet::PACKET_SIZE + encryption::AES256GCM_VER_TAG_SIZE];
    if let Err(e) = stream.read_exact(&mut buffer) {
        return Err(format!("Failed to read hash from stream: {e}"));
    }

    let file_hash_packet = match encryption::decrypt_message(nonce, cipher, &buffer) {
        Ok(h) => h,
        Err(e) => return Err(format!("Failed to decrypt ciphertext: {e}"))
    };

    let file_hash_packet = match packet::decode_packet(file_hash_packet) {
        Ok(p) => p,
        Err(e) => return Err(format!("Unable to decode packet: {e}"))
    };

    // figure out what file was requested
    let file_hash = hex::encode(file_hash_packet.data);
    let file_path = match get_file_from_catalog(&file_hash) {
        Ok(p) => p,
        Err(e) => return Err(format!("Failed to get file from catalog: {e}"))
    };

    // send peer file name to properly save the file and size to know how many bytes to expect
    if let Err(e) = send_file_metadata(&file_path, &cipher, nonce, stream) {
        return Err(format!("Failed to send file name and hash to peer: {e}"));
    }

    let file = match File::open(&file_path) {
        Ok(f) => f,
        Err(e) => return Err(format!("Couldn't open file: {e}"))
    };

    let mmap = match unsafe { Mmap::map(&file) } {
        Ok(m) => m,
        Err(e) => return Err(format!("Couldn't open memory map: {e}"))
    };

    println!("Sending {:?} to {:?}...", file_path.file_name().unwrap(), stream.peer_addr().unwrap());

    // subtract 2 for the data_length bytes
    for chunk in mmap.chunks(packet::PACKET_SIZE - 2) {
        // encode the data and send the packet
        let message = packet::encode_packet(chunk.to_vec());
        if let Err(e) = encryption::send_to_connection(&mut stream, &mut nonce, &cipher, message) {
            return Err(format!("{e}"));
        }
    }

    println!("Successfully sent {:?} to {:?}", file_path.file_name().unwrap(), stream.peer_addr().unwrap());

    Ok(())
}



/// An asynchronous task that handles sending a file over `stream`
pub async fn start_sender_task(mut stream: TcpStream) {
    // carry out DH exchange
    let dh_private_key = EphemeralSecret::random_from_rng(&mut OsRng);
    let dh_public_key = PublicKey::from(&dh_private_key);

    // send public key to listener
    if let Err(e) = stream.write_all(dh_public_key.as_bytes()) {
        eprintln!("Failed to send DH public key: {e}");
        return;
    }

    // wait for public key response from listener
    let mut public_key_bytes: [u8; 32] = [0; 32];
    match stream.read_exact(&mut public_key_bytes) {
        Ok(_) => (),
        Err(e) if e.kind() == ErrorKind::UnexpectedEof => return, // indicates ping was sent, so do not continue connection
        Err(e) => {
            eprintln!("Failed to read peer's public key: {e}");
            return;
        }
    };

    // generate and store AES cipher
    let peer_public_key = PublicKey::from(public_key_bytes);
    let dh_shared_secret = dh_private_key.diffie_hellman(&peer_public_key);
    let key = Key::<Aes256Gcm>::from_slice(dh_shared_secret.as_bytes());
    let cipher = Aes256Gcm::new(key);
    let mut nonce = [0u8; 12];

    // listen for the mode packet sent
    let mut buffer = [0u8; packet::PACKET_SIZE + encryption::AES256GCM_VER_TAG_SIZE];
    if let Err(e) = stream.read_exact(&mut buffer) {
        eprintln!("Failed to read from stream: {e}");
        return;
    }

    let mode_packet = match encryption::decrypt_message(&mut nonce, &cipher, &buffer) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Failed to decrypt ciphertext of mode packet: {e}");
            return;
        }
    };

    let mode_packet = match packet::decode_packet(mode_packet) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Unable to decode mode packet: {e}");
            return;
        }
    };

    // split tasks depending on mode sent by requester
    match String::from_utf8(mode_packet.data) {
        Ok(m) if m == "request_catalog" => {
            if let Err(e) = fulfill_catalog_request(&mut stream, &mut nonce, &cipher) {
                eprintln!("Failed to fulfill catalog request: {e}");
            } else {
                println!("Catalog sent to {:?}", stream.peer_addr().unwrap())
            }
        },
        Ok(m) if m == "request_file" => {
            if let Err(e) = fulfill_file_request(&mut stream, &mut nonce, &cipher) {
                eprintln!("Failed to fulfill file request: {e}");
            }
        },
        Ok(_) => (),
        Err(e) => {
            eprintln!("Failed to read mode: {e}");
            return;
        }
    }
}



pub fn start_listening() {
    // Create and enter a new async runtime
    let runtime = Runtime::new().expect("Failed to create a runtime");
    let _ = runtime.enter();

    println!("Starting listener...");
    let listen_addr = String::from("0.0.0.0:7878");
    let listener = match TcpListener::bind(&listen_addr) {
        Ok(l) => {
            println!("Client listening on {}", &listen_addr);
            l
        },
        Err(e) => {
            eprintln!("Failed to bind: {}", e);
            return;
        }
    };
    println!("Successfully started listener");

    // start handling incoming connections
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Failed to accept connection: {e}");
                continue;
            }
        };

        // spawn a new task for each incoming stream to handle more than one connection
        runtime.spawn(start_sender_task(stream));
    }
}