use std::path::Path;

fn main() {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo supplies the manifest directory");
    let locales = Path::new(&manifest).join("locales");
    println!("cargo:rerun-if-changed={}", locales.display());
    let mut paths: Vec<_> = std::fs::read_dir(&locales)
        .expect("bundled locales directory")
        .map(|entry| entry.expect("bundled locale entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "ftl"))
        .collect();
    paths.sort();
    let output = std::env::var_os("OUT_DIR").expect("Cargo supplies OUT_DIR");
    let mut source = String::from("pub const SOURCES: &[(&str, &str)] = &[\n");
    for path in paths {
        println!("cargo:rerun-if-changed={}", path.display());
        let language = path.file_stem().and_then(|name| name.to_str()).expect("UTF-8 locale name");
        let bundled = if std::env::var_os("CARGO_FEATURE_FULL").is_some() {
            path.clone()
        } else {
            let text = std::fs::read_to_string(&path).expect("bundled Fluent source");
            let mut resource = fluent_syntax::parser::parse(text.as_str()).expect("bundled Fluent syntax");
            resource.body.retain(|entry| match entry {
                fluent_syntax::ast::Entry::Message(message) => {
                    ["language-", "number-", "currency-", "common-", "login-", "session-", "tui-"]
                        .iter()
                        .any(|prefix| message.id.name.starts_with(prefix))
                }
                fluent_syntax::ast::Entry::Term(_) => true,
                _ => false,
            });
            let bundled = Path::new(&output).join(path.file_name().expect("locale file name"));
            std::fs::write(&bundled, fluent_syntax::serializer::serialize(&resource)).expect("write chat catalog");
            bundled
        };
        source.push_str(&format!("({language:?}, include_str!({:?})),\n", bundled.to_string_lossy()));
    }
    source.push_str("];\n");
    std::fs::write(Path::new(&output).join("locales.rs"), source).expect("write bundled locale index");
}
