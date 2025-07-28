fn main() -> std::io::Result<()> {
    let fds = protox::compile(["helloworld.proto"], ["."]).unwrap();
    tonic_prost_build::compile_fds(fds)
}
