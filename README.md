# infoclimat-om-worker

Service Rust d'agrégation d'OMfiles Open-Meteo. Pour une requête « cumul de
précipitations sur N heures », télécharge en parallèle les N OMfiles horaires
depuis `map-tiles.open-meteo.com`, somme les grilles pixel-à-pixel (NaN
propagés), réencode un OMfile cumulé qui préserve la compression et les
métadonnées d'origine, et cache le résultat dans R2.

Conçu pour être consommé directement par
[`@openmeteo/weather-map-layer`](https://github.com/open-meteo/typescript-omfiles)
via le protocole `om://`, sans modification du pipeline de rendu existant.

## Architecture

```
Browser ── om:// ──► weather-map-layer ──HTTP──► infoclimat-om-worker
                                                       │
                                                       ├──► R2 (cache)
                                                       │
                                                       └──► map-tiles.open-meteo.com
                                                              (N OMfiles horaires)
```

- **In** : 1 requête HTTP par tuile (côté maps).
- **Out** : 1 OMfile binaire, déterministe, cacheable à vie (un run passé est immuable).
- **Cache** : clé R2 = `v1/{domain}/{output_var}/{hours}h/{sha256(...)[:16]}.om`.

## Endpoints

### `GET /v1/sum/:domain/:base_variable/:hours_segment/:Y/:M/:D/:HHMMZ/:time.om`

Endpoint **path-style**, compatible avec le pipeline `omProtocol` du
`weather-map-layer` (qui strip la query-string avant de fetch). C'est l'URL que
le client maps construit pour les variables `*_sum_*h`.

Exemple :

```
GET /v1/sum/meteofrance_arpege_europe/precipitation/24h/2026/05/23/0000Z/2026-05-23T1800.om
```

- `domain` — identifiant de domaine Open-Meteo (`meteofrance_arpege_europe`, `dwd_icon`, …).
- `base_variable` — variable horaire à sommer (typiquement `precipitation`).
- `hours_segment` — `Nh` où N ∈ [1, 336].
- `Y/M/D/HHMMZ` — date/heure du **run** du modèle.
- `time.om` — fin de fenêtre ; le cumul couvre `[time − hours + 1h, time]`.

Le nom de la variable dans l'OMfile retourné est `{base_variable}_sum_{hours}h`
— c'est ce nom que le client doit passer dans `?variable=` pour la rendre.

### `GET /v1/sum?domain=…&variable=…&run=…&time=…&hours=…&output_variable=…`

Endpoint **query-style**, pour tests CLI / scripts ad-hoc.

| Param              | Exemple                          | Description                                              |
|--------------------|----------------------------------|----------------------------------------------------------|
| `domain`           | `meteofrance_arpege_europe`      | Identifiant de domaine Open-Meteo                        |
| `variable`         | `precipitation`                  | Variable horaire à sommer                                |
| `run`              | `2026-05-23T00:00:00Z`           | Run du modèle (RFC 3339)                                 |
| `time`             | `2026-05-23T18:00:00Z`           | Fin de fenêtre (RFC 3339)                                |
| `hours`            | `24`                             | Taille de la fenêtre en heures (1 à 336)                 |
| `output_variable`  | `precipitation_sum_24h`          | Optionnel ; défaut `{variable}_sum_{hours}h`             |

Réponse identique à la route path-style.

### `GET /healthz`

`200 OK` si le service répond.

### `GET /v1/sum_since_0h/:domain/:base_variable/:Y/:M/:D/:HHMMZ/:time.om`

Cumul de `base_variable` depuis 00:00 UTC du jour de `time` (inclus) jusqu'à
`time` (inclus). Pré-condition : le `run` doit être à 00:00 UTC du même jour
que `time` — sinon `400 Bad Request`.

Exemple :

```
GET /v1/sum_since_0h/meteofrance_arome_france_hd/precipitation/2026/05/23/0000Z/2026-05-23T1500.om
```

Le nom de variable dans l'OMfile retourné est `{base_variable}_sum_since_0h`.

## Headers de réponse

| Header          | Valeur                                       |
|-----------------|----------------------------------------------|
| `Content-Type`  | `application/octet-stream`                   |
| `Cache-Control` | `public, max-age=31536000, immutable`        |
| `X-Cache`       | `HIT` (servi depuis R2) ou `MISS` (calculé) |

## Variables d'environnement

| Var                       | Défaut                              | Description                                |
|---------------------------|-------------------------------------|--------------------------------------------|
| `LISTEN_ADDR`             | `0.0.0.0:8080`                      | Bind HTTP                                  |
| `OPENMETEO_BASE_URL`      | `https://map-tiles.open-meteo.com`  | Source des OMfiles horaires                |
| `S3_ENDPOINT`             | -                                   | Endpoint R2 : `https://<ACCOUNT_ID>.r2.cloudflarestorage.com` |
| `S3_REGION`               | `us-east-1`                         | Pour R2, mettre `auto`                     |
| `S3_BUCKET`               | `om-cumul-cache`                    | Bucket R2 de cache                         |
| `S3_ACCESS_KEY_ID`        | -                                   | R2 API token — Access Key ID               |
| `S3_SECRET_ACCESS_KEY`    | -                                   | R2 API token — Secret                      |
| `MAX_CONCURRENT_FETCHES`  | `16`                                | Parallélisme du download des sources       |
| `RUST_LOG`                | `info`                              | Niveau de log (ex: `info,infoclimat_om_worker=debug`) |

Un fichier `.env` à la racine est auto-chargé au démarrage via
[`dotenvy`](https://crates.io/crates/dotenvy). Voir `.env.example` pour le
template.

## Setup local

1. **Créer le bucket R2** (une fois) :

   ```bash
   npx wrangler r2 bucket create om-cumul-cache
   ```

2. **Générer un token API R2** dans le dashboard Cloudflare
   (R2 → *Manage R2 API Tokens* → *Create API Token*, scope *Object Read & Write*
   sur le bucket `om-cumul-cache`). Noter l'Access Key ID et le Secret.

3. **Copier le template `.env`** :

   ```bash
   cp .env.example .env
   # éditer .env avec l'ACCOUNT_ID Cloudflare et les credentials R2
   ```

4. **Lancer** :

   ```bash
   cargo run --release
   # ou pour itérer
   cargo run
   ```

5. **Tester** :

   ```bash
   curl 'http://localhost:8080/healthz'
   curl -o cumul.om \
     'http://localhost:8080/v1/sum/meteofrance_arpege_europe/precipitation/3h/2026/05/22/0000Z/2026-05-22T0300.om'
   ```

## Build

```bash
# Binaire natif
cargo build --release

# Image Docker (multi-stage, ~80 Mo)
docker build -t infoclimat-om-worker:dev .
docker run --rm -p 8080:8080 --env-file .env infoclimat-om-worker:dev

# Ou via compose
docker compose up --build
```

Le build natif requiert `clang`, `libclang-dev`, `build-essential` pour
compiler `om-file-format-sys` (bindings C → bindgen).

## Intégration côté maps

Le client `maps/` (SvelteKit) est déjà câblé. Il suffit de définir l'URL du
worker au build :

```bash
# dans le repo maps/, créer .env.local
echo "VITE_OM_WORKER_URL=http://localhost:8080" > .env.local
npm run dev
```

Puis ouvrir une URL avec une variable cumul, par exemple :

```
http://localhost:5173/?domain=meteofrance_arpege_europe&variable=precipitation_sum_3h&model_run=2026-05-22T0000&time=2026-05-22T0300
```

Côté maps, les changements faits pour activer le routing :

- `src/lib/url.ts` — `getOMUrl()` détecte le pattern `^(.+)_sum_(\d+)h$` et
  construit une URL vers le worker au lieu du bucket Open-Meteo.
- `src/lib/stores/om-protocol-settings.ts` — `resolveRequest` customisé qui
  parse le domaine depuis le path `/v1/sum/{domain}/…` (le résolveur par
  défaut attend `/data_spatial/{domain}/…`).
- `src/lib/metadata.ts` — `matchVariableOrFirst()` skip les variables cumul
  pour éviter qu'elles ne soient remappées sur la variable de base.

À noter : les variables cumul n'apparaissent pas (encore) dans le dropdown
UI ; il faut les sélectionner via le paramètre `?variable=` de l'URL. Pour
les exposer dans le menu il faudrait soit forker
`@openmeteo/weather-map-layer`, soit ajouter un override local de
`variableOptions`.

## Limites et pièges connus

- **Pas de validation du `time_interval` du domaine.** La sommation suppose un
  pas horaire. Sur un modèle 3-horaire (`cma_grapes_global`, etc.) elle
  produirait un cumul 3× trop long sans erreur. À fixer avant d'élargir le
  whitelist au-delà des modèles Europe/France horaires.
- **Pas de validation des variables disponibles.** Le worker tente de lire
  `base_variable` dans le multi-variable OMfile amont ; si absent, retourne
  HTTP 422 avec un message explicite — mais ne le détecte qu'après avoir
  téléchargé le premier fichier.
- **Pas d'auth, pas de rate-limit.** OK pour un POC interne, à durcir avant
  exposition publique.
- **NaN propagés.** Si un pixel est manquant dans l'une des grilles sources,
  le pixel cumulé est NaN. Comportement souhaité (on ne ment pas sur des
  données manquantes), mais peut surprendre.
- **Rétention amont 7 jours.** `map-tiles.open-meteo.com` supprime les
  OMfiles spatiaux après 7 jours (header `x-amz-expiration` sur les
  réponses). Au-delà, les requêtes cumul retournent 502.
- **`sum_since_0h` ne couvre pas les heures avant 00 UTC.** Pour avoir un cumul
  depuis 00 UTC à n'importe quelle heure de la journée, le run 00 UTC du jour
  doit être disponible amont. Avant cette publication, le client doit retomber
  sur `sum_Nh` avec un autre run de référence.

## Roadmap

- [ ] Whitelist domaines + variables avec leur `time_interval`.
- [ ] Validation pré-fetch via `meta.json` du domaine (cache R2 séparé).
- [ ] Métriques Prometheus (`/metrics`).
- [ ] Tests d'intégration (mock 2-3 OMfiles → vérifier somme + relecture).
- [ ] CI GitHub Actions (build, test, push image).
- [ ] Rate-limit (`tower-governor`) si exposition publique.
- [ ] Pré-calcul batch des cumuls des runs récents pour éviter les MISS
      pendant les heures de pointe.

## Stack

- **Rust 1.85+** (édition 2024).
- [`axum`](https://docs.rs/axum) — HTTP server.
- [`omfiles`](https://github.com/open-meteo/rust-omfiles) — lecteur/écrieur OMfile officiel Open-Meteo.
- [`aws-sdk-s3`](https://docs.rs/aws-sdk-s3) — client R2 (S3-compatible).
- [`reqwest`](https://docs.rs/reqwest) — client HTTP pour les sources Open-Meteo.

## Licence

MIT ou Apache-2.0, au choix.
