use std::{env, fs, io, path::Path};

fn main() -> io::Result<()> {
    let manifest = env::var_os("CARGO_MANIFEST_DIR").unwrap();
    let dist = Path::new(&manifest).join("../../apps/web/dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    let output = env::var_os("OUT_DIR").unwrap();
    let output = Path::new(&output);
    let mut assets = String::from("static EMBEDDED: &[(&str, &[u8])] = &[\n");
    if dist.join("index.html").is_file() {
        collect(&dist, &dist, &output.join("web"), &mut assets)?;
    }
    assets.push_str("];\n");
    fs::write(output.join("assets.rs"), assets)
}

fn collect(root: &Path, dir: &Path, output: &Path, assets: &mut String) -> io::Result<()> {
    let mut entries = fs::read_dir(dir)?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect(root, &path, output, assets)?;
        } else if kind.is_file() {
            let relative = path.strip_prefix(root).unwrap();
            let url = relative.to_string_lossy().replace('\\', "/");
            let snapshot = output.join(relative);
            fs::create_dir_all(snapshot.parent().unwrap())?;
            fs::copy(&path, &snapshot)?;
            assets.push_str(&format!("({url:?}, include_bytes!({snapshot:?})),\n"));
        }
    }
    Ok(())
}
