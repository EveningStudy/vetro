use std::{env, fs, io, path::Path};

fn main() -> io::Result<()> {
    let manifest = env::var_os("CARGO_MANIFEST_DIR").unwrap();
    let dist = Path::new(&manifest).join("../../apps/web/dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    let mut assets = String::from("static EMBEDDED: &[(&str, &[u8])] = &[\n");
    if dist.join("index.html").is_file() {
        collect(&dist, &dist, &mut assets)?;
    }
    assets.push_str("];\n");
    fs::write(
        Path::new(&env::var_os("OUT_DIR").unwrap()).join("assets.rs"),
        assets,
    )
}

fn collect(root: &Path, dir: &Path, assets: &mut String) -> io::Result<()> {
    let mut entries = fs::read_dir(dir)?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect(root, &path, assets)?;
        } else if kind.is_file() {
            let url = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            assets.push_str(&format!("({url:?}, include_bytes!({path:?})),\n"));
        }
    }
    Ok(())
}
