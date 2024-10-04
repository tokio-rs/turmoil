fn main() -> std::io::Result<()> {
    // avoids having a local install of protoc
    std::env::set_var("PROTOC", protobuf_src::protoc());
    tonic_build::compile_protos("helloworld.proto")
}
