use eyre::{bail, ensure, eyre, Result};
use oauth2::basic::{BasicClient, BasicTokenResponse};
use oauth2::reqwest::http_client;
use oauth2::{
    AuthType, AuthUrl, AuthorizationCode, ClientId, CsrfToken, PkceCodeChallenge, RedirectUrl,
    Scope, TokenUrl,
};
use tiny_http::{Method, Request, Response, Server, StatusCode};
use url::Url;

fn extract_authorization_code<'a>(
    url: &'a Url,
    csrf_token: &CsrfToken,
) -> Result<std::borrow::Cow<'a, str>> {
    // Looking for
    // /redirect?code=Mac..dc6&state=DL7jz5YIW4WusaYdDZrXzA%3d%3d
    let mut received_code = None;
    let mut received_state = None;
    for pair in url.query_pairs() {
        match pair.0.as_ref() {
            "code" => {
                ensure!(received_code.is_none(), "Duplicate code");
                received_code = Some(pair.1);
            }
            "state" => {
                ensure!(received_state.is_none(), "Duplicate state");
                received_state = Some(pair.1);
            }
            parameter => {
                bail!("Unexpected parameter: {} {}", parameter, pair.1.as_ref());
            }
        }
    }
    match received_state {
        None => {
            bail!("No CSRF token received");
        }
        Some(state) => {
            ensure!(state.as_ref() == csrf_token.secret(), "CSRF token mismatch");
        }
    }
    match received_code {
        None => Err(eyre!("No authorization code received")),
        Some(code) => Ok(code),
    }
}

fn handle_request(request: Request, csrf_token: &CsrfToken) -> Result<String> {
    let err = match request.method() {
        Method::Get => {
            let base = Url::parse("http://localhost:3003/")?;
            let url = base.join(request.url())?;
            if url.path() == "/redirect" {
                match extract_authorization_code(&url, &csrf_token) {
                    Ok(code) => {
                        let response = Response::from_string("You may now close this window.");
                        if let Err(respond_err) = request.respond(response) {
                            eprintln!("Error sending HTTP response: {}", respond_err);
                        }
                        return Ok(code.into_owned());
                    }
                    Err(err) => err,
                }
            } else {
                eyre!("Unrecognized path: {}", request.url())
            }
        }
        _ => eyre!("Unsupported method: {}", request.method()),
    };
    let status_code = StatusCode(404);
    let response =
        Response::from_string(status_code.default_reason_phrase()).with_status_code(status_code);
    if let Err(respond_err) = request.respond(response) {
        eprintln!("Error sending HTTP response: {}", respond_err);
    }
    Err(err)
}

fn get_authorization_code(server: &Server, csrf_token: CsrfToken) -> Result<String> {
    for request in server.incoming_requests() {
        match handle_request(request, &csrf_token) {
            Ok(code) => {
                return Ok(code);
            }
            Err(err) => {
                eprintln!("Error handling HTTP request: {}", err);
            }
        }
    }

    Err(eyre!(
        "No more incoming connections and auth code not supplied",
    ))
}
fn start_server() -> eyre::Result<Server> {
    tiny_http::Server::http("127.0.0.1:0").map_err(|e| eyre!(e))
}

pub fn authenticate(client_id: String) -> Result<BasicTokenResponse> {
    let ms_graph_authorize_url =
        AuthUrl::new("https://login.microsoftonline.com/common/oauth2/v2.0/authorize".to_string())?;
    let ms_graph_token_url = Some(TokenUrl::new(
        "https://login.microsoftonline.com/common/oauth2/v2.0/token".to_string(),
    )?);

    let server = start_server()?;
    let redirect_url = format!("http://localhost:{}/redirect", server.server_addr().port());

    let client = BasicClient::new(
        ClientId::new(client_id),
        None,
        ms_graph_authorize_url,
        ms_graph_token_url,
    )
    .set_auth_type(AuthType::RequestBody)
    .set_redirect_uri(RedirectUrl::new(redirect_url)?);

    // Setup PKCE code challenge
    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

    // Generate the full authorization URL.
    let (auth_url, csrf_token) = client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new("Files.Read.All".to_string()))
        .set_pkce_challenge(pkce_challenge)
        .url();

    if let Err(e) = open::that(auth_url.as_str()) {
        println!("{}", e);
        println!("Browse to {}", auth_url);
    }

    let authorization_code = get_authorization_code(&server, csrf_token)?;

    // close down server
    drop(server);

    let token_result = client
        .exchange_code(AuthorizationCode::new(authorization_code))
        .set_pkce_verifier(pkce_verifier)
        .request(http_client)?;

    Ok(token_result)
}
