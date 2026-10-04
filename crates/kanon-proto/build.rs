fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut protos = vec!["../../proto/kanon/v1/plugin.proto"];
    if std::env::var_os("CARGO_FEATURE_DSH").is_some() {
        protos.push("../../proto/kanon/v1/agent.proto");
    }
    tonic_build::configure().compile_protos(&protos, &["../../proto"])?;
    Ok(())
}
