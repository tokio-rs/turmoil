fn main() {
    tonic_build::compile_protos("helloworld.proto").unwrap();
}
