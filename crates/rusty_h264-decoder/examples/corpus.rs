//! Corpus classifier: runs the decoder over a directory of `.264` streams and
//! reports, for each, whether it decoded, was gracefully rejected (with the
//! reason), or **panicked** (the hardening target — a panic on untrusted input
//! is a bug). Used for the decoder gap analysis.
//!
//! Usage: cargo run -p rusty_h264-decoder --example corpus -- <dir-of-.264>

use rusty_h264_decoder::Decoder;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Mutex;

static LAST_PANIC: Mutex<String> = Mutex::new(String::new());

fn main() {
    let dir = std::env::args().nth(1).expect("usage: corpus <dir>");
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("read dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "264"))
        .collect();
    entries.sort();

    let (mut ok, mut rejected, mut panicked) = (0, 0, 0);
    let mut panic_files = Vec::new();
    // Capture the panic location+message instead of printing the default hook.
    std::panic::set_hook(Box::new(|info| {
        let loc = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        *LAST_PANIC.lock().unwrap() = format!("{loc}  {msg}");
    }));

    for path in &entries {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let data = std::fs::read(path).expect("read file");
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut dec = Decoder::new();
            // Feed the whole stream; decode() splits NALs and keeps ref state.
            // A multi-picture stream returns only the last frame, but every
            // slice is still decoded — enough to surface crashes.
            dec.decode(&data)
        }));
        match result {
            Ok(Ok(Some(_frame))) => {
                ok += 1;
                println!("  DECODED  {name}");
            }
            Ok(Ok(None)) => {
                ok += 1;
                println!("  NOFRAME  {name}  (parsed, no coded picture returned)");
            }
            Ok(Err(e)) => {
                rejected += 1;
                println!("  REJECT   {name}  -> {e}");
            }
            Err(_) => {
                panicked += 1;
                let where_ = LAST_PANIC.lock().unwrap().clone();
                panic_files.push(name.clone());
                println!("  PANIC!!  {name}  @ {where_}");
            }
        }
    }

    let _ = std::panic::take_hook();
    println!(
        "\n{} files: {ok} decoded/parsed, {rejected} gracefully rejected, {panicked} PANICKED",
        entries.len()
    );
    if !panic_files.is_empty() {
        println!("panics: {}", panic_files.join(", "));
    }
}
