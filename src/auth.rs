use std::error::Error;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use string_error;
use oauth2::prelude::*;
use oauth2::{
    AuthorizationCode,
    AuthType,
    AuthUrl,
    ClientId,
    ClientSecret,
    CsrfToken,
    PkceCodeVerifierS256,
    RedirectUrl,
    ResponseType,
    Scope,
    TokenUrl
};
use oauth2::basic::{BasicClient, BasicTokenResponse, BasicRequestTokenError};
use open;
use rand::{thread_rng, Rng};
use tiny_http::{Server, Response, Method, StatusCode};
use url::Url;

fn extract_authorization_code(url: &Url, csrf_token: &CsrfToken) -> Option<String> {
    // Looking for
    // /redirect?code=Mac..dc6&state=DL7jz5YIW4WusaYdDZrXzA%3d%3d
    let mut received_code = None;
    let mut received_state = None;
    for pair in url.query_pairs() {
        match pair.0.as_ref() {
            "code" => {
                if received_code.is_some() {
                    println!("Duplicate code");
                    return None;
                }
                received_code = Some(pair.1.into_owned());
            },
            "state" => {
                if received_state.is_some() {
                    println!("Duplicate state");
                    return None;
                }
                received_state = Some(pair.1);
            },
            parameter => {
                println!("Unexpected parameter: {}", parameter);
                return None;
            }
        }
    }
    if received_state.as_ref().unwrap() != csrf_token.secret() {
        println!("CSRF token mismatch");
        return None;
    }
    if received_code.is_none() {
        println!("No authorization code received");
    }
    return received_code;
}

fn get_authorization_code(
    server: &Server,
    csrf_token: CsrfToken,
) -> Result<String, io::Error> {

    for request in server.incoming_requests() {
        let status_code = match request.method() {
            Method::Get => {
                let base = Url::parse("http://localhost:3003/").unwrap();
                let url = base.join(request.url()).unwrap();
                if url.path() == "/redirect" {
                    match extract_authorization_code(&url, &csrf_token) {
                        None => StatusCode(404),
                        Some(code) => {
                            let response = Response::from_string("You may now close this window.");
                            request.respond(response)?;
                            return Ok(code)
                        }
                    }
                }
                else {
                    println!("Unrecognized path: {}", request.url());
                    StatusCode(404)
                }
            },
            _ => {
                println!("Unsupported method: {}", request.method());
                StatusCode(404)
            }
        };

        let response = Response::from_string(status_code.default_reason_phrase())
            .with_status_code(status_code);
        request.respond(response)?;
    }

    panic!("No more incoming connections and auth code not supplied")
}

fn start_server() -> Result<Server, Box<dyn Error>> {
    // MS Graph requires an exact match for the redirect URL. To reduce the chance of failing
    // if a fixed port is in use, we try 8 different ports. The MS app must have 8 registered
    // Redirect URI's: http://localhost:<port>/redirect for each value of <port>
    let mut ports: [u16; 8] = [3003, 17465, 22496, 23620, 25243, 27194, 28207, 32483];
    // Select ports in random order to prevent herding and add a bit of security through
    // non-deterministic behavior.
    let mut rng = thread_rng();
    rng.shuffle(&mut ports);
    let mut socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 0);
    for port in &ports {
        socket.set_port(*port);
        match Server::http(socket) {
            Ok(server) => return Ok(server),
            Err(err) => {
                match err.downcast::<io::Error>() {
                    Ok(io_err) => {
                        if io_err.kind() == io::ErrorKind::AddrInUse {
                            // try next port
                        }
                        else {
                            return Err(io_err);
                        }
                    }
                    Err(err) => {
                        return Err(err);
                    }
                }
            }
        }
    }
    Err(string_error::static_err("Could not find an available port"))
}

pub fn authenticate(client_id: String, client_secret: String)
    -> Result<BasicTokenResponse, BasicRequestTokenError>
{
    let ms_graph_authorize_url = AuthUrl::new(
        Url::parse("https://login.microsoftonline.com/common/oauth2/v2.0/authorize").unwrap()
    );
    let ms_graph_token_url = Some(
        TokenUrl::new(
            Url::parse("https://login.microsoftonline.com/common/oauth2/v2.0/token").unwrap()
        )
    );

    let server = start_server().unwrap();
    let redirect_url = format!("http://localhost:{}/redirect", server.server_addr().port());

    let client =
        BasicClient::new(
            ClientId::new(client_id),
            Some(ClientSecret::new(client_secret)),
            ms_graph_authorize_url,
            ms_graph_token_url
        )
        .set_auth_type(AuthType::RequestBody)
        .add_scope(Scope::new("Files.Read.All".to_string()))
        .set_redirect_url(RedirectUrl::new(Url::parse(&redirect_url).unwrap()));

    // Setup PKCE code challenge
    let code_verifier = PkceCodeVerifierS256::new_random();

    // Generate the full authorization URL.
    let (auth_url, csrf_token) = client.authorize_url_extension(
        &ResponseType::new("code".to_string()),
        CsrfToken::new_random,
        &code_verifier.authorize_url_params(),
    );

    if let Err(e) = open::that(auth_url.as_str()) {
        println!("{}", e);
        println!("Browse to {}", auth_url);
    }

    let authorization_code = get_authorization_code(&server, csrf_token).unwrap();

    // close down server
    drop(server);

    // Send the PKCE code verifier in the token request
    let params: Vec<(&str, &str)> = vec![("code_verifier", &code_verifier.secret())];

    client.exchange_code_extension(AuthorizationCode::new(authorization_code), &params)
}
