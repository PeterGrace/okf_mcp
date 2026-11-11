# ChatGPT's client metadata document is accepted (issue #273)

## Fixed

- ChatGPT could not connect over OAuth: `/oauth/authorize` answered "This
  client is not known to the server" for `client_id
  https://chatgpt.com/oauth/client.json`. Its metadata document declares
  `token_endpoint_auth_method: private_key_jwt` while listing `none` in
  `token_endpoint_auth_methods_supported`, and asks for the
  `urn:ietf:params:oauth:grant-type:jwt-bearer` grant. Client metadata
  (CIMD documents and DCR registrations) is now accepted when the client
  can use `none`, either declared or listed among its supported methods. It
  is still treated as a public client (PKCE, exact redirect URI, no client
  authentication). Unsupported grant types are dropped instead of refused,
  and `authorization_code` is required.

## Changed

- `/oauth/authorize` logs why it refused a client, at `warn`
  (`refused an OAuth client`, with the `client_id` and the reason). The page
  the person sees is unchanged.
- A registration or document whose only grant types are unsupported, or that
  leaves out `authorization_code` (for example `["refresh_token"]` alone),
  is now refused. It could never have started a sign-in.

## Notes

- Regression test `a_client_that_also_offers_none_is_accepted_as_public`
  uses ChatGPT's document verbatim (fetched 2026-11-11). It failed before the
  fix with `token_endpoint_auth_method "private_key_jwt" is not supported`.
