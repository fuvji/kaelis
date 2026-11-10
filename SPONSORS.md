# Sponsoring Kaelis

Kaelis is free, needs no account, and collects no telemetry. Sponsorship is the
only revenue the project takes, and the product page carries no third-party ad
script, cookie, or affiliate redirect. A sponsor is a labelled card, served from
the same HTML as the rest of the page, and it never sits between a visitor and a
download.

## Inventory

| Placement | Where | Format |
| --- | --- | --- |
| Sponsor card | `#support` on the product page | Name, one sentence, one link, optional 64×64 mark |
| Release credit | The GitHub release notes for one tagged version | One line and a link |
| README credit | `README.md`, Sponsors section | One line and a link |

One sponsor per placement per release cycle. Placements are reviewed by hand:
Minecraft servers, mods, tooling, and developer products are welcome; gambling,
game-file resellers, and anything that breaks Mojang's EULA are not.

## How to buy a placement

Open a GitHub issue titled `Sponsorship: <name>` at
<https://github.com/fuvji/kaelis/issues/new> with the name, URL, one sentence of
copy, and the release you want. There is no self-serve checkout and no minimum
term; a single release is fine. Payment goes through GitHub Sponsors so the
project never handles card data.

## Adding a sponsor to the page

The card lives in the `#support` section of `site/index.html`. Replace the
`sponsor-slot` block's heading, sentence, and link, keep the `rel="noreferrer"`
attribute, and add `data-sponsor="<name>"` so the placement is easy to audit.
Do not add remote scripts, pixels, or iframes: the Content-Security-Policy for
the page allows none, and a placement that needs one is declined.

## Why not an ad network

Networks such as EthicalAds and Carbon Ads pay per impression and require a
traffic floor this project does not yet publish. Their scripts would also be the
first third-party request on a page that currently makes none, which contradicts
the product's own privacy claim. If traffic ever justifies it, the decision gets
its own note in the vault and a visible disclosure here before any script ships.

## Current status

- GitHub Sponsors: enabled through `.github/FUNDING.yml`.
- Direct sponsor slot: open, unoccupied.
- Ad network: none, by choice.
