use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str;
use std::error::Error;
use wasm_bindgen::prelude::*;
use turmoil::Builder;
use turmoil::net;
use turmoil::net::IpAddr as TurmoilIpAddr;
use turmoil::net::SocketAddr as TurmoilSocketAddr;
use turmoil::net::{TcpStream, TcpListener, UdpSocket};

// Initialize panic hook for better error messages
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

#[wasm_bindgen]
pub fn test_std_ip_addr_functionality() -> bool {
    // Test IPv4 functionality
    let localhost_v4 = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    if !localhost_v4.is_loopback() || localhost_v4.is_unspecified() {
        return false;
    }
    
    // Test IPv6 functionality
    let localhost_v6 = IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1));
    if !localhost_v6.is_loopback() || localhost_v6.is_unspecified() {
        return false;
    }
    
    true
}

#[wasm_bindgen]
pub fn test_turmoil_ip_addr() -> bool {
    // Test Turmoil's IPv4 functionality
    let localhost_v4 = TurmoilIpAddr::V4(turmoil::net::Ipv4Addr::new(127, 0, 0, 1));
    if !localhost_v4.is_loopback() || localhost_v4.is_unspecified() {
        return false;
    }
    
    // Test Turmoil's IPv6 functionality
    let localhost_v6 = TurmoilIpAddr::V6(turmoil::net::Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1));
    if !localhost_v6.is_loopback() || localhost_v6.is_unspecified() {
        return false;
    }
    
    true
}

#[wasm_bindgen]
pub fn test_turmoil_socket_addr() -> String {
    // Test Turmoil's IPv4 SocketAddr functionality
    let socket_v4 = TurmoilSocketAddr::new(
        TurmoilIpAddr::V4(turmoil::net::Ipv4Addr::new(127, 0, 0, 1)), 
        8080
    );
    
    if socket_v4.port() != 8080 {
        return format!("IPv4 socket port test failed: expected 8080, got {}", socket_v4.port());
    }
    
    if !socket_v4.is_ipv4() {
        return "IPv4 socket is_ipv4() test failed: expected true".to_string();
    }
    
    if socket_v4.is_ipv6() {
        return "IPv4 socket is_ipv6() test failed: expected false".to_string();
    }
    
    // Test Turmoil's IPv6 SocketAddr functionality
    let socket_v6 = TurmoilSocketAddr::new(
        TurmoilIpAddr::V6(turmoil::net::Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1)), 
        8080
    );
    
    if socket_v6.port() != 8080 {
        return format!("IPv6 socket port test failed: expected 8080, got {}", socket_v6.port());
    }
    
    if socket_v6.is_ipv4() {
        return "IPv6 socket is_ipv4() test failed: expected false".to_string();
    }
    
    if !socket_v6.is_ipv6() {
        return "IPv6 socket is_ipv6() test failed: expected true".to_string();
    }
    
    // Test string display for socket addresses
    let socket_v4_str = socket_v4.to_string();
    let socket_v6_str = socket_v6.to_string();
    
    if socket_v4_str != "127.0.0.1:8080" {
        return format!("IPv4 socket string representation test failed: expected '127.0.0.1:8080', got '{}'", socket_v4_str);
    }
    
    if socket_v6_str != "[::1]:8080" {
        return format!("IPv6 socket string representation test failed: expected '[::1]:8080', got '{}'", socket_v6_str);
    }
    
    "success".to_string()
}

// Regular tests
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_ip_addresses() {
        assert!(test_std_ip_addr_functionality());
        assert!(test_turmoil_ip_addr());
        let socket_result = test_turmoil_socket_addr();
        assert_eq!(socket_result, "success", "SocketAddr test failed: {}", socket_result);
    }
    
    #[test]
    fn test_all_tcp_udp_dns() {
        let tcp_result = test_tcp_functionality();
        assert_eq!(tcp_result, "success", "TCP test failed: {}", tcp_result);
        
        let udp_result = test_udp_functionality();
        assert_eq!(udp_result, "success", "UDP test failed: {}", udp_result);
        
        let dns_result = test_dns_functionality();
        assert_eq!(dns_result, "success", "DNS test failed: {}", dns_result);
        
        let builder_result = test_builder_config();
        assert_eq!(builder_result, "success", "Builder test failed: {}", builder_result);
    }
}

// Helper function to create an error
fn make_error(msg: String) -> Box<dyn Error + 'static> {
    Box::new(std::io::Error::new(std::io::ErrorKind::Other, msg))
}

// Test basic TCP functionality - simplified version
#[wasm_bindgen]
pub fn test_tcp_functionality() -> String {
    let mut sim = Builder::new().build();
    
    // Just test that we can create a TCP socket and the simulation doesn't crash
    sim.host("test", || async {
        // Create a socket address
        let addr = TurmoilSocketAddr::new(
            TurmoilIpAddr::V4(net::Ipv4Addr::new(127, 0, 0, 1)),
            8080
        );
        
        // Try to bind a listener
        let _listener = TcpListener::bind(addr).await?;
        
        // Success
        Ok(())
    });
    
    // Run the simulation
    match sim.run() {
        Ok(_) => "success".to_string(),
        Err(e) => format!("TCP test failed: {}", e),
    }
}

// Test basic UDP functionality - simplified version
#[wasm_bindgen]
pub fn test_udp_functionality() -> String {
    let mut sim = Builder::new().build();

    // Just test that we can create a UDP socket
    sim.host("test", || async {
        let addr = TurmoilSocketAddr::new(
            TurmoilIpAddr::V4(net::Ipv4Addr::new(127, 0, 0, 1)),
            8080
        );
        
        // Try to bind a socket
        let _socket = UdpSocket::bind(addr).await?;
        
        Ok(())
    });
    
    // Run the simulation
    match sim.run() {
        Ok(_) => "success".to_string(),
        Err(e) => format!("UDP test failed: {}", e),
    }
}

// Test builder configuration
#[wasm_bindgen]
pub fn test_builder_config() -> String {
    let builder = Builder::new();
    let mut sim = builder.build();
    
    sim.host("test", || async {
        Ok(())
    });
    
    match sim.run() {
        Ok(_) => "success".to_string(),
        Err(e) => format!("Builder configuration test failed: {}", e),
    }
}

// Test DNS functionality
#[wasm_bindgen]
pub fn test_dns_functionality() -> String {
    let mut sim = Builder::new().build();
    
    // Test resolving the DNS entry
    sim.client("client", async {
        // Initialize a host so it gets an IP
        let host_addr = turmoil::lookup("server");
        
        // Test TcpStream::connect with hostname
        let stream_result = TcpStream::connect(("server", 80)).await;
        if stream_result.is_err() {
            return Err(make_error(format!("Failed to connect using hostname: {}", stream_result.err().unwrap())));
        }
        
        Ok(())
    });
    
    // Set up a server
    sim.host("server", || async {
        let addr = TurmoilSocketAddr::new(
            TurmoilIpAddr::V4(net::Ipv4Addr::new(192, 168, 0, 1)),
            80
        );
        
        // Bind to port 80
        let listener = match TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => return Err(make_error(format!("Failed to bind server on port 80: {}", e))),
        };
        
        // Accept a connection (from client)
        let (_socket, _) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => return Err(make_error(format!("Failed to accept connection: {}", e))),
        };
        
        Ok(())
    });
    
    match sim.run() {
        Ok(_) => "success".to_string(),
        Err(e) => format!("DNS test failed: {}", e),
    }
}

// Wasm tests for running in browser
#[cfg(all(target_arch = "wasm32", test))]
mod wasm_tests {
    use super::*;
    use wasm_bindgen_test::*;
    
    wasm_bindgen_test_configure!(run_in_browser);
    
    #[wasm_bindgen_test]
    fn test_ip_addresses_wasm() {
        assert!(test_std_ip_addr_functionality());
        assert!(test_turmoil_ip_addr());
        let socket_result = test_turmoil_socket_addr();
        assert_eq!(socket_result, "success", "SocketAddr test failed: {}", socket_result);
    }
    
    #[wasm_bindgen_test]
    fn test_tcp_in_wasm() {
        let result = test_tcp_functionality();
        assert_eq!(result, "success", "TCP test failed: {}", result);
    }
    
    #[wasm_bindgen_test]
    fn test_udp_in_wasm() {
        let result = test_udp_functionality();
        assert_eq!(result, "success", "UDP test failed: {}", result);
    }
    
    #[wasm_bindgen_test]
    fn test_builder_in_wasm() {
        let result = test_builder_config();
        assert_eq!(result, "success", "Builder test failed: {}", result);
    }
    
    #[wasm_bindgen_test]
    fn test_dns_in_wasm() {
        let result = test_dns_functionality();
        assert_eq!(result, "success", "DNS test failed: {}", result);
    }
}