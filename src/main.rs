mod request;
mod response;

use clap::Parser;
use rand::{Rng, SeedableRng};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

#[derive(Parser, Debug)]
#[command(about = "Command Options")]
struct CmdOptions {
    #[arg(short, long, default_value = "0.0.0.0:1100")]
    bind: String,
    // Upstream host to forward requests to.
    #[arg(short, long)]
    upstream: Vec<String>,
    // Perform active health checks on this interval (in seconds)
    #[arg(long, default_value = "10")]
    active_health_check_interval: usize,
    // Path to send request to for active health checks.
    #[arg(long, default_value = "/")]
    active_health_check_path: String,
    // Maximum number of requests to accept per IP per minute (0 = unlimited)
    #[arg(long, default_value = "0")]
    max_requests_per_minute: usize,
}

struct ProxyState {
    // How frequently we check whether upstream servers are alive
    #[allow(dead_code)]
    active_health_check_interval: usize,
    // Where we should send requests when doing active health checks
    #[allow(dead_code)]
    active_health_check_path: String,
    // Maximum number of requests an individual IP can make in a minute
    #[allow(dead_code)]
    max_requests_per_minute: usize,
    // Addresses of servers that we are proxying to
    upstream_addresses: Vec<String>,
}

#[tokio::main]
async fn main() {
    if let Err(_) = std::env::var("RUST_LOG") {
        std::env::set_var("RUST_LOG", "debug");
    }
    pretty_env_logger::init();

    let options = CmdOptions::parse();
    if options.upstream.len() < 1 {
        log::error!("At least one upstream server must be specified using the --upstream option.");
        std::process::exit(1);
    }

    let listener = match TcpListener::bind(&options.bind).await {
        Ok(listener) => listener,
        Err(err) => {
            log::error!("Could not bind to {}: {}", options.bind, err);
            std::process::exit(1);
        }
    };
    log::info!("Listening for requests on {}", options.bind);

    let state = Arc::new(ProxyState {
        upstream_addresses: options.upstream,
        active_health_check_interval: options.active_health_check_interval,
        active_health_check_path: options.active_health_check_path,
        max_requests_per_minute: options.max_requests_per_minute,
    });

    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(e) => {
                log::error!("Failed to accept new connection: {}", e);
                std::process::exit(1);
            }
        };

        let state = state.clone();
        tokio::spawn(handle_connection(stream, state));
    }
}

// Open a connection to a random destination server
async fn connect_to_upstream(state: &ProxyState) -> Result<TcpStream, std::io::Error> {
    let mut rng = rand::rngs::StdRng::from_entropy();
    let upstream_idx = rng.gen_range(0..state.upstream_addresses.len());
    let upstream_ip = &state.upstream_addresses[upstream_idx];
    TcpStream::connect(upstream_ip).await.or_else(|err| {
        log::error!("Failed to connect to upstream {}: {}", upstream_ip, err);
        Err(err)
    })

    // TODO: implement failover
}

async fn send_response(client_conn: &mut TcpStream, response: &http::Response<Vec<u8>>) {
    let client_ip = client_conn.peer_addr().unwrap().ip().to_string();
    log::info!(
        "{} <- {}",
        client_ip,
        response::format_response_line(&response)
    );

    if let Err(err) = response::write_to_stream(&response, client_conn).await {
        log::warn!("Failed to send response to client: {}", err);
        return;
    }
}

async fn handle_connection(mut client_conn: TcpStream, state: Arc<ProxyState>) {
    let client_ip = client_conn.peer_addr().unwrap().ip().to_string();
    log::info!("Connection received from {client_ip}");

    let mut upstream_conn = match connect_to_upstream(state.as_ref()).await {
        Ok(stream) => stream,
        Err(_) => {
            let response = response::make_http_error(http::StatusCode::BAD_GATEWAY);
            send_response(&mut client_conn, &response).await;
            return;
        }
    };
    let upstream_ip = upstream_conn.peer_addr().unwrap().ip().to_string();

    // The client may now send us one or more requests. Keep trying to read requests until the
    // client hangs up or we get an error.
    loop {
        // Read a request from the client
        let mut request = match request::read_from_stream(&mut client_conn).await {
            Ok(request) => request,
            // Handle case where client closed connection and is no longer sending requests.
            Err(request::Error::IncompleteRequest(0)) => {
                log::debug!("Client finished sending requests. Shutting down connection");
                return;
            }
            // Handle I/O error in reading from the client
            Err(request::Error::ConnectionError(io_err)) => {
                log::info!("Error reading request from client stream: {}", io_err);
                return;
            }
            Err(error) => {
                log::debug!("Error parsing request: {:?}", error);
                let response = response::make_http_error(match error {
                    request::Error::IncompleteRequest(_)
                    | request::Error::MalformedRequest(_)
                    | request::Error::InvalidContentLength
                    | request::Error::ContentLengthMismatch => http::StatusCode::BAD_REQUEST,
                    request::Error::RequestBodyTooLarge => http::StatusCode::PAYLOAD_TOO_LARGE,
                    request::Error::ConnectionError(_) => http::StatusCode::SERVICE_UNAVAILABLE,
                });
                send_response(&mut client_conn, &response).await;
                continue;
            }
        };
        log::info!(
            "{} -> {}: {}",
            client_ip,
            upstream_ip,
            request::format_request_line(&request)
        );

        // Add X-Forwarded-For header so that the upstream server knows the client's IP address.
        request::extend_header_value(&mut request, "x-forwarded-for", &client_ip);

        // Forward the request to the server
        if let Err(error) = request::write_to_stream(&request, &mut upstream_conn).await {
            log::error!(
                "Failed to send request to upstream {}: {}",
                upstream_ip,
                error
            );
            let response = response::make_http_error(http::StatusCode::BAD_GATEWAY);
            send_response(&mut client_conn, &response).await;
            return;
        }
        log::debug!("Forwarded request to server");

        // Read the server's response
        let response = match response::read_from_stream(&mut upstream_conn, request.method()).await
        {
            Ok(response) => response,
            Err(error) => {
                log::error!("Error reading response from server: {:?}", error);
                let response = response::make_http_error(http::StatusCode::BAD_GATEWAY);
                send_response(&mut client_conn, &response).await;
                return;
            }
        };

        // Forward the response to the client
        send_response(&mut client_conn, &response).await;
        log::debug!("Forwarded response to client");
    }
}
