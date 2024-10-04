use prost::Message;
use std::path::PathBuf;

fn main() -> std::io::Result<()> {
    let file_descriptors = protox::compile(["helloworld.proto"], ["."]).unwrap();
    let file_descriptor_path = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR not set"))
        .join("file_descriptor_set.bin");
    std::fs::write(&file_descriptor_path, file_descriptors.encode_to_vec()).unwrap();

    tonic_build::configure()
        .file_descriptor_set_path(&file_descriptor_path)
        .skip_protoc_run()
        .compile_protos(&["helloworld.proto"], &["."])
        .unwrap();

    Ok(())
}
