fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::compile_protos("../../proto/kanon/v1/plugin.proto")?;
    Ok(())
}
