# Bitmagnet HTTPS Virtual Host Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deploy Bitmagnet on `51.89.6.57` and securely publish its GraphQL endpoint at `https://bitmagnet.aigutta.com` without interrupting Discourse.

**Architecture:** Bitmagnet and PostgreSQL run on a dedicated Docker bridge network, with no host publication for the GraphQL or database ports. The existing Discourse container joins that same network at creation time, so its Nginx can resolve the Bitmagnet service by name. The current Let's Encrypt template obtains one SAN certificate for the forum hostname and `bitmagnet.aigutta.com`.

**Tech Stack:** Docker Compose, Bitmagnet `v0.10.1`, PostgreSQL 16 Alpine, Discourse Docker launcher, Nginx, Let's Encrypt, Cloudflare proxy.

**Spec:** User-approved virtual-host design in this conversation; server discovery recorded in this plan.

## Global Constraints

- Keep Discourse bound to host ports 80 and 443; do not introduce a second host-level reverse proxy.
- Do not publish TCP 3333 or PostgreSQL 5432 on the host.
- Publish Bitmagnet DHT port 3334 for both TCP and UDP.
- Only accept GraphQL requests that Cloudflare identifies as originating from `199.19.109.120`.
- Keep Bitmagnet data and its generated database password outside the application repository.
- Use `ghcr.io/bitmagnet-io/bitmagnet:v0.10.1`; upgrades are deliberate, backed up and validated changes.

---

### Task 1: Verify DNS and prepare persistent Bitmagnet configuration

**Files:**
- Create: `/opt/bitmagnet/.env`
- Create: `/opt/bitmagnet/docker-compose.yml`
- Create: `/opt/bitmagnet/config/`
- Create: `/opt/bitmagnet/data/postgres/`

**Interfaces:**
- Consumes: Docker on `51.89.6.57`; `bitmagnet.aigutta.com` resolves through Cloudflare to this server.
- Produces: a `bitmagnet` container on the dedicated `bitmagnet-net` Docker network and a `bitmagnet-postgres` dependency, reachable only by containers on that network.

- [x] **Step 1: Confirm DNS reaches the server before requesting a SAN certificate**

Run:

```bash
getent ahostsv4 bitmagnet.aigutta.com
curl --resolve bitmagnet.aigutta.com:80:51.89.6.57 -I http://bitmagnet.aigutta.com/
```

Expected: the hostname resolves through Cloudflare and the direct-origin request reaches the existing web service. Do not rebuild Discourse if the hostname does not resolve.

- [x] **Step 2: Create a private Compose environment file with a generated database password**

Run:

```bash
sudo install -d -m 0700 /opt/bitmagnet/config /opt/bitmagnet/data/postgres
sudo sh -c 'umask 077; printf "POSTGRES_PASSWORD=%s\n" "$(openssl rand -hex 32)" > /opt/bitmagnet/.env'
sudo chmod 0600 /opt/bitmagnet/.env
```

- [x] **Step 3: Write the minimal Compose stack**

Create `/opt/bitmagnet/docker-compose.yml` with this exact content:

```yaml
services:
  bitmagnet:
    image: ghcr.io/bitmagnet-io/bitmagnet:v0.10.1
    container_name: bitmagnet
    restart: unless-stopped
    env_file: .env
    environment:
      POSTGRES_HOST: bitmagnet-postgres
    volumes:
      - ./config:/root/.config/bitmagnet
    ports:
      - "3334:3334/tcp"
      - "3334:3334/udp"
    command:
      - worker
      - run
      - --keys=http_server
      - --keys=queue_server
      - --keys=dht_crawler
    depends_on:
      bitmagnet-postgres:
        condition: service_healthy
    networks:
      - bitmagnet-net

  bitmagnet-postgres:
    image: postgres:16-alpine
    container_name: bitmagnet-postgres
    restart: unless-stopped
    env_file: .env
    environment:
      POSTGRES_DB: bitmagnet
      PGUSER: postgres
    volumes:
      - ./data/postgres:/var/lib/postgresql/data
    shm_size: 1g
    healthcheck:
      test: ["CMD-SHELL", "pg_isready"]
      interval: 10s
      timeout: 5s
      start_period: 20s
      retries: 6
    networks:
      - bitmagnet-net

networks:
  bitmagnet-net:
    name: bitmagnet-net
```

- [x] **Step 4: Validate the Compose model before starting containers**

Run:

```bash
cd /opt/bitmagnet
sudo docker compose config
```

Expected: only ports 3334/tcp and 3334/udp appear in the rendered configuration; 3333 and 5432 do not appear as published ports.

- [x] **Step 5: Start and verify the data services**

Run:

```bash
cd /opt/bitmagnet
sudo docker compose up -d
sudo docker compose ps
sudo docker logs --tail=100 bitmagnet
```

Expected: PostgreSQL is healthy and Bitmagnet is running its `http_server`, `queue_server` and `dht_crawler` workers.

### Task 2: Add the persistent Discourse Nginx virtual host

**Files:**
- Create: `/var/discourse/templates/bitmagnet.template.yml`
- Modify: `/var/discourse/containers/app.yml`
- Backup: `/var/discourse/containers/app.yml.before-bitmagnet`

**Interfaces:**
- Consumes: the running Docker container named `bitmagnet` on the `bitmagnet-net` bridge; the Discourse `app` container; `/shared/ssl/forum.aigutta.com.{cer,key}` generated by the existing Let's Encrypt template.
- Produces: HTTP-to-HTTPS redirection and an HTTPS `server_name bitmagnet.aigutta.com` Nginx virtual host proxying to `http://bitmagnet:3333`.

- [x] **Step 1: Create the launcher template that generates both server blocks**

Create `/var/discourse/templates/bitmagnet.template.yml` with this exact content:

```yaml
run:
  - file:
      path: /etc/nginx/conf.d/outlets/before-server/10-bitmagnet.conf
      chmod: 0644
      contents: |
        server {
          listen 80;
          listen [::]:80;
          server_name bitmagnet.aigutta.com;

          location ~ /.well-known {
            root /var/www/discourse/public;
            allow all;
          }

          location / {
            return 301 https://bitmagnet.aigutta.com$request_uri;
          }
        }

        server {
          listen 443 ssl;
          listen [::]:443 ssl;
          http2 on;
          server_name bitmagnet.aigutta.com;

          ssl_protocols TLSv1.2 TLSv1.3;
          ssl_certificate /shared/ssl/forum.aigutta.com.cer;
          ssl_certificate /shared/ssl/forum.aigutta.com_ecc.cer;
          ssl_certificate_key /shared/ssl/forum.aigutta.com.key;
          ssl_certificate_key /shared/ssl/forum.aigutta.com_ecc.key;

          real_ip_header cf-connecting-ip;
          real_ip_recursive on;
          include /etc/nginx/conf.d/outlets/server/set-real-ip-from-cloudflare.conf;

          location / {
            allow 199.19.109.120;
            deny all;

            proxy_http_version 1.1;
            proxy_set_header Host $host;
            proxy_set_header X-Real-IP $remote_addr;
            proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
            proxy_set_header X-Forwarded-Proto https;
            proxy_pass http://bitmagnet:3333;
          }
        }
```

- [x] **Step 2: Back up and update the Discourse launcher configuration**

Run:

```bash
sudo cp -a /var/discourse/containers/app.yml /var/discourse/containers/app.yml.before-bitmagnet
```

Under `templates:`, add:

```yaml
  - "templates/bitmagnet.template.yml"
```

Under `env:`, add:

```yaml
  DISCOURSE_HOSTNAME_ALIASES: "bitmagnet.aigutta.com"
```

At the top level, add this Docker network argument:

```yaml
docker_args: "--network bitmagnet-net"
```

- [x] **Step 3: Apply and validate the virtual host without another full rebuild**

The launcher updated itself during the initial rebuild and stopped the old container before the SSH session ended. The old container was restarted and PostgreSQL completed its automatic recovery. To avoid a second unnecessary outage, attach the running `app` container to the named network, request the SAN certificate through its existing `letsencrypt` helper, then copy the rendered Nginx server block into the container.

Run:

```bash
sudo docker network connect bitmagnet-net app
sudo docker exec -e DISCOURSE_HOSTNAME_ALIASES=bitmagnet.aigutta.com app /usr/local/bin/letsencrypt
sudo docker exec app nginx -t
sudo docker exec app sv reload nginx
sudo docker exec app getent hosts bitmagnet
```

Expected: `nginx -t` reports successful syntax, Docker DNS resolves `bitmagnet` from the `app` container, and the installed certificate SAN set contains both HTTPS hostnames. The already-saved launcher template, alias and `docker_args` apply the same configuration on the next routine Discourse rebuild.

- [x] **Step 4: Verify the hostname routing and access boundary**

Run from `51.89.6.57`:

```bash
curl --resolve bitmagnet.aigutta.com:443:127.0.0.1 -k -I https://bitmagnet.aigutta.com/graphql
```

Run from `199.19.109.120`:

```bash
curl -fsS -X POST https://bitmagnet.aigutta.com/graphql \
  -H 'content-type: application/json' \
  --data '{"query":"{ __typename }"}'
```

Expected: the origin-side test is denied because it is not the allowed upstream source; the metadata host reaches the GraphQL endpoint and receives a GraphQL response rather than an HTTP gateway error.

### Task 3: Connect the metadata service and verify end-to-end resolution

**Files:**
- Modify: `/opt/magnet-metadata-api/docker-compose.slim.yaml` or its referenced environment file on `199.19.109.120`

**Interfaces:**
- Consumes: `https://bitmagnet.aigutta.com/graphql`, available only to `199.19.109.120` through Cloudflare.
- Produces: the `torrent-metadata-api` process configured with `BITMAGNET_URL=https://bitmagnet.aigutta.com`.

- [x] **Step 1: Back up the active metadata-service Compose configuration**

Run:

```bash
cd /opt/magnet-metadata-api
cp -a docker-compose.slim.yaml docker-compose.slim.yaml.before-bitmagnet
```

- [x] **Step 2: Add the provider endpoint without duplicating `/graphql`**

In the `torrent-api` service environment, add:

```yaml
BITMAGNET_URL: https://bitmagnet.aigutta.com
```

The metadata service appends `/graphql` itself when the configured URL does not already end in that path.

- [x] **Step 3: Recreate only the metadata service and check provider connectivity**

Run:

```bash
cd /opt/magnet-metadata-api
docker compose -f docker-compose.slim.yaml up -d --force-recreate torrent-api
docker logs --tail=100 torrent-metadata-api
curl -fsS -X POST http://127.0.0.1:8080/test/bitmagnet
```

Expected: the healthcheck remains healthy and the provider test no longer reports a missing `BITMAGNET_URL` variable.

- [x] **Step 4: Resolve the supplied magnet through the public metadata API**

Run:

```bash
curl -fsS -X POST http://127.0.0.1:8080/api/v1/metadata \
  -H 'content-type: application/json' \
  --data '{"magnet_uri":"magnet:?xt=urn:btih:F4646AE88E5C971D4AEDCE22CCAC49578DAE2EA1"}'
```

Expected: a JSON response is returned. If Bitmagnet has not yet indexed this hash, the existing iTorrents/local fallback remains eligible and the logs show an explicit provider miss rather than a missing-endpoint error.

### Task 4: Document operational checks and rollback

**Files:**
- Create: `/opt/bitmagnet/OPERATIONS.md`

**Interfaces:**
- Consumes: the deployment paths and container names created above.
- Produces: repeatable backup, update, health-check and rollback commands for the service owner.

- [x] **Step 1: Record backup and health-check commands**

Write `/opt/bitmagnet/OPERATIONS.md` containing:

```markdown
# Bitmagnet Operations

## Health

```bash
cd /opt/bitmagnet && sudo docker compose ps
sudo docker logs --tail=100 bitmagnet
```

## Database backup

```bash
sudo docker exec bitmagnet-postgres pg_dump -U postgres bitmagnet | gzip > /opt/bitmagnet/backup-$(date +%F).sql.gz
```

## Upgrade

```bash
cd /opt/bitmagnet
sudo docker exec bitmagnet-postgres pg_dump -U postgres bitmagnet | gzip > backup-before-upgrade.sql.gz
sudo docker compose pull bitmagnet
sudo docker compose up -d bitmagnet
```
```

- [x] **Step 2: Record and verify the server rollback sequence**

Run:

```bash
sudo cp -a /var/discourse/containers/app.yml.before-bitmagnet /var/discourse/containers/app.yml
cd /var/discourse && sudo ./launcher rebuild app
```

Use this only if the virtual-host rebuild fails validation or interrupts forum routing. It restores the pre-change launcher configuration; then stop the Bitmagnet stack with `cd /opt/bitmagnet && sudo docker compose down` if its removal is also required.
