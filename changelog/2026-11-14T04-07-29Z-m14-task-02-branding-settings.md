# M14 Task 2: branding settings (issue #392)

## Added

- Two runtime settings, `product_name` (default `okf`; 1 to 60 characters,
  trimmed, no control characters) and `support_contact` (default `none`; an
  email address or an `https://` URL with a host). Both go through
  `okf-mcp settings set`, `PUT /api/v1/settings/{key}` and the Settings page,
  like the policy settings.
- `store::settings::Brand` (`Serialize`), `Settings::brand()`,
  `settings::brand_in(conn)` and the constants `DEFAULT_PRODUCT_NAME` and
  `MAX_PRODUCT_NAME_CHARS`.
- `GET /ui/brand.json`: public, `no-cache`, under the console's security
  headers; `{"product_name", "support_contact"}` (`null` when unset).
- `applyBrand()` in the console shell: the product name on the login card,
  in the sidebar and in the tab title, at boot and after the setting is saved
  or reset.
- Tests: the settings' checks and listing, the defaults, every template's
  product name and contact line, the default brand rendering as before, the
  server page shell, `brand.json`, a mail queued after a change, the sign-in
  pages and the consent pages through the router, and the browser journey
  `the_product_name_set_on_the_settings_page_brands_the_console`.

## Changed

- Mail: `render(template, brand)` names the product in every subject and body
  and appends "Questions? Contact ..." when a contact is set; `enqueue_in`
  reads the brand on the caller's connection, so mail queued by any server or
  the CLI has the stored brand.
- Server pages: `pages::document` and `pages::page` take the brand (title,
  brand line, and a contact line after the body, a URL as a link);
  `scripted_page` is replaced by `pages::respond(status, document, csp)`.
  The sign-in, forgot, setup, reset, OIDC message and OAuth consent pages pass
  `&app.settings().brand()`.
- `docs/operations.md`: the Settings table's two rows and the
  `/ui/brand.json` route.

## Notes

- The defaults render exactly as before (`okf`, no contact line).
- The plan's corrections for this task are dated notes in Task 2, plus one
  for Task 9a (`render` now adds the contact line to every template).
