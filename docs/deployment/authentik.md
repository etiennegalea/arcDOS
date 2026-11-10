# Authentik and ArcDOS

ArcDOS uses Authentik as its OpenID Connect provider. In this example the browser reaches ArcDOS at `https://drive.example.com` and Authentik at `https://auth.example.com`. Use your own hostnames. Configure HTTPS for both services and route the complete ArcDOS origin through the reverse proxy to the ArcDOS server. Keep the upstream port private, preserve the request paths, and keep the callback query parameters out of access logs. ArcDOS must reach the Authentik hostname over HTTPS with a publicly trusted certificate.

## Users, groups, and live claims

Create two groups in Authentik under **Directory → Groups**. Assign people directly to the groups they need.

| Group | ArcDOS access |
| --- | --- |
| `drive-users` | Browse and change files on the shared filesystem |
| `drive-admins` | Shared filesystem access and authority to revoke ArcDOS web sessions |

In **Customization → Property Mappings**, create an **OAuth2/OpenID Scope Mapping** with scope name `arcdos` and the expression below. Add the mapping to each ArcDOS provider. ArcDOS requests `openid arcdos`. Authentik supplies the subject in UserInfo and evaluates the mapping against the current account on each call. [Scope mappings](https://docs.goauthentik.io/add-secure-apps/providers/oauth2/#scope-mappings) · [Provider expressions](https://docs.goauthentik.io/add-secure-apps/providers/property-mappings/expression/) · [User properties](https://docs.goauthentik.io/users-sources/user/user_ref/)

```python
return {
    "arcdos_active": user.is_active,
    "arcdos_groups": list(
        user.groups.filter(name__in=["drive-users", "drive-admins"])
        .values_list("name", flat=True)
    ),
    "arcdos_display_name": user.name or user.username,
}
```

ArcDOS requires an active account and at least one matching group for every file request. Create an Authentik Expression Policy and bind it to each ArcDOS application. The policy checks group access at the application and requires S256 PKCE on OAuth authorization requests. [Application policy bindings](https://docs.goauthentik.io/add-secure-apps/applications/manage_apps/#use-bindings-to-control-access) · [OAuth policy context](https://github.com/goauthentik/authentik/blob/main/authentik/providers/oauth2/views/authorize.py#L467-L475)

```python
if "oauth_grant_type" in request.context:
    if not request.context.get("oauth_code_challenge"):
        return False
    if request.context.get("oauth_code_challenge_method") != "S256":
        return False

return request.user.is_active and request.user.groups.filter(
    name__in=["drive-users", "drive-admins"]
).exists()
```

## Password, MFA, and passkeys

1. Create a TOTP Authenticator Setup stage and a WebAuthn Authenticator Setup stage. WebAuthn supports passkeys and hardware security keys. For passkeys, keep a stable HTTPS Authentik hostname and choose a user-verification policy for your deployment. [TOTP setup](https://docs.goauthentik.io/add-secure-apps/flows-stages/stages/authenticator_totp/) · [WebAuthn setup](https://docs.goauthentik.io/add-secure-apps/flows-stages/stages/authenticator_webauthn/)
2. Create or duplicate an **Authentication** flow with **Identification**, **Password**, **Authenticator Validation**, and **User Login** in that order. Configure **Authenticator Validation** for `totp` and `webauthn`, set **Not configured action** to **Configure**, and attach both setup stages so a person can enroll a factor at first login. Use a zero **Last validation threshold** to require validation every time this authentication flow runs. Check that stage policies require validation after password authentication. [Authenticator validation](https://docs.goauthentik.io/add-secure-apps/flows-stages/stages/authenticator_validate/)
3. Select the flow on each ArcDOS provider. Test from a private window with an enrolled user and a new user. ArcDOS web requests `prompt=login` so Authentik runs the configured authentication flow even when the browser has an SSO session. [Default flows](https://docs.goauthentik.io/add-secure-apps/flows-stages/flow/default-flows/) · [Authentik reauthentication implementation](https://github.com/goauthentik/authentik/blob/main/authentik/providers/oauth2/views/authorize.py#L538-L550)

## Confidential web provider

Create an Authentik application and OAuth2/OIDC provider together. Use a slug such as `arcdos-web`. Select the MFA authentication flow, an authorization flow such as Authentik's explicit-consent flow, and the provider invalidation flow. [Create an OAuth2 provider](https://docs.goauthentik.io/add-secure-apps/providers/oauth2/create-oauth2-provider/)

| Setting | Value |
| --- | --- |
| Client type | Confidential |
| Grant type | Authorization Code only |
| Redirect URI | Strict match for `https://drive.example.com/auth/callback` |
| Issuer mode | Per provider, based on the application slug |
| Subject mode | Stable user identifier, such as Authentik's hashed user ID |
| Signing key | RSA certificate and key pair for RS256 |
| Encryption key | Leave blank for a signed ID token |
| Scope mappings | The `arcdos` mapping for the `openid arcdos` request |

Copy the client ID and save the client secret in a private file. Use the exact issuer from the provider's discovery document, including the trailing slash. For the example, the issuer is `https://auth.example.com/application/o/arcdos-web/`. ArcDOS fetches its discovery document and signing keys at startup. Restart ArcDOS after rotating the provider signing key. The browser starts authorization code flow with S256 PKCE, and the ArcDOS backend exchanges the code. ArcDOS sessions expire after 30 minutes without a session-authenticated request, at provider token expiry, or eight hours after sign-in, whichever comes first. Sign in again to start a new session. [Authentik OAuth2 provider settings](https://docs.goauthentik.io/add-secure-apps/providers/oauth2/)

## Start ArcDOS

Set the following variables on the Linux server or in its container environment.

| Variable | Example or purpose |
| --- | --- |
| `ARCDOS_DATA_DIR` | `/data` on a private, writable Linux mount |
| `ARCDOS_PUBLIC_ORIGIN` | `https://drive.example.com` |
| `ARCDOS_OIDC_ISSUER` | `https://auth.example.com/application/o/arcdos-web/` |
| `ARCDOS_OIDC_CLIENT_ID` | The confidential web provider's client ID |
| `ARCDOS_OIDC_CLIENT_SECRET_FILE` | Absolute path to the confidential secret file |

Leave `ARCDOS_DEV_TOKEN_FILE` unset in Authentik mode. Keep the client secret file outside the data and web directories. The image runs as UID and GID 10001. Give that user access to the writable data mount and a read-only secret file.

On a Linux Docker host with a reverse proxy running on the host, prepare `/srv/arcdos/data` for UID 10001 and store the secret in `/etc/arcdos/oidc-client-secret` with owner 10001 and mode 0400. Replace the example hostnames and client ID, then run the following from the repository root.

```sh
docker build -f deploy/Dockerfile.server -t arcdos:local .
docker run --name arcdos --restart unless-stopped \
  --read-only --cap-drop=ALL --security-opt=no-new-privileges \
  -p 127.0.0.1:8081:8080 \
  --mount type=bind,source=/srv/arcdos/data,target=/data \
  --mount type=bind,source=/etc/arcdos/oidc-client-secret,target=/run/arcdos-oidc-secret,readonly \
  -e ARCDOS_DATA_DIR=/data \
  -e ARCDOS_PUBLIC_ORIGIN=https://drive.example.com \
  -e ARCDOS_OIDC_ISSUER=https://auth.example.com/application/o/arcdos-web/ \
  -e ARCDOS_OIDC_CLIENT_ID=replace-with-web-client-id \
  -e ARCDOS_OIDC_CLIENT_SECRET_FILE=/run/arcdos-oidc-secret \
  arcdos:local
```

Route `https://drive.example.com` to `http://127.0.0.1:8081` from that proxy. Open `https://drive.example.com/files/` and choose **Sign in with Authentik**. The public health check reports process readiness. The file API requires a valid identity and current group access.

## Logout and revocation

- **Sign out** invalidates the ArcDOS browser session and asks Authentik to revoke its access token. To end the separate Authentik SSO session, sign out from Authentik. [Authentik logout flows](https://docs.goauthentik.io/add-secure-apps/flows-stages/flow/default-flows/)
- A member of `drive-admins` can revoke a subject's existing ArcDOS browser sessions with `POST /auth/admin/revoke`. Include the session cookie, exact same-origin `Origin`, `X-CSRF-Token`, `Content-Type: application/json`, and a body such as `{"subject":"example-oidc-subject"}`. A user who still has group access can sign in again.
- To withdraw access in Authentik, remove both ArcDOS group memberships, disable the account, or revoke its OAuth access tokens. ArcDOS checks current UserInfo on the next protected request. The file API denies access while Authentik is unavailable.

## Android public provider

To enable the ArcDOS API's Android bearer-token support, create a second Authentik application and **Public** OAuth2/OIDC provider with a distinct client ID, per-provider issuer on the same Authentik origin, an RSA signing key, the `arcdos` scope mapping, and the same MFA and group policy. Enable Authorization Code and Refresh Token on the Android provider. Register the app's strict redirect URI and add the `offline_access` scope mapping. [Android setup](../android.md) · [OAuth for native apps](https://www.rfc-editor.org/rfc/rfc8252.html)

Public clients can omit `prompt=login` and reuse an Authentik SSO session. Require MFA in every Authentik login flow that can establish an SSO session for ArcDOS users. Invalidate any existing sessions created under weaker flows before enabling the Android provider.

On the **Android** provider, add the **web** provider under **Federated OAuth2/OpenID Providers**. This lets the confidential web client introspect Android tokens. Set both `ARCDOS_ANDROID_ISSUER` and `ARCDOS_ANDROID_CLIENT_ID` on ArcDOS, then restart it. ArcDOS checks active status, issuer, client, audience, expiry, scopes, and live UserInfo before allowing access to files. [Federated introspection](https://docs.goauthentik.io/add-secure-apps/providers/oauth2/#cross-provider-token-introspection-and-revocation)
