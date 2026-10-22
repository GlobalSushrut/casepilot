use std::env;
use std::fs;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-env-changed=CONNECTOR_OS_TARBALL");
    let Ok(source) = env::var("CONNECTOR_OS_TARBALL") else {
        return;
    };
    let from = Path::new(&source);
    if !from.is_file() {
        return;
    }
    let dest_dir = Path::new("deploy").join("bundle");
    let _ = fs::create_dir_all(&dest_dir);
    let _ = fs::copy(from, dest_dir.join("connector-os.tar.gz"));
}
