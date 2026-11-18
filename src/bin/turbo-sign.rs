//! Gera a chave Ed25519 e assina o instalador.
//! A chave secreta não entra no repositório.

use std::env;
use std::fs;
use std::io::Read;

use rand_core::{OsRng, RngCore};
use turbo::updater::{public_key_from_secret, sha256_hex, sign_bytes};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    match cmd {
        "generate" => {
            let mut seed = [0u8; 32];
            OsRng.fill_bytes(&mut seed);
            let secret = hex::encode(seed);
            let public = public_key_from_secret(&secret).expect("chave pública");
            let out = args.get(1).cloned().unwrap_or_else(|| "turbo-signing-key.txt".into());
            fs::write(&out, format!("{secret}\n")).expect("gravar chave");
            println!("chave secreta: {out}");
            println!("chave pública: {public}");
            println!("grave a chave pública em releases/update-public-key.txt e recompile.");
        }
        "public" => {
            let key = flag(&args, "--key");
            let secret = fs::read_to_string(&key).expect("ler chave");
            println!("{}", public_key_from_secret(secret.trim()).expect("chave pública"));
        }
        "sign" => {
            let key = flag(&args, "--key");
            let file = flag(&args, "--file");
            let secret = fs::read_to_string(&key).expect("ler chave").lines().next().unwrap_or("").trim().to_string();
            let mut bytes = Vec::new();
            fs::File::open(&file).expect("abrir arquivo").read_to_end(&mut bytes).expect("ler");
            let signature = sign_bytes(&bytes, &secret).expect("assinar");
            let hash = sha256_hex(&bytes);
            let name = std::path::Path::new(&file).file_name().unwrap().to_string_lossy();
            println!("{hash}  {name}");
            println!("# signature {name} {signature}");
        }
        _ => {
            eprintln!("uso:\n  turbo-sign generate [arquivo-da-chave]\n  turbo-sign public --key arquivo\n  turbo-sign sign --key arquivo --file TurboApp-Setup.exe");
            std::process::exit(2);
        }
    }
}

fn flag(args: &[String], name: &str) -> String {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned().unwrap_or_else(|| {
        eprintln!("falta {name}");
        std::process::exit(2);
    })
}
