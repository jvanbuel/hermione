# Hermione infrastructure

OpenTofu that deploys Hermione on Scaleway: a Kapsule (managed Kubernetes)
cluster, a Managed PostgreSQL instance, and Argo CD reconciling a Helm chart
from this repository.

```
                       internet
                          │
                  ┌───────▼────────┐   reserved flexible IP, adopted by the
                  │ Scaleway LB    │   Scaleway CCM via a Service annotation
                  └───────┬────────┘
                          │
                  ┌───────▼────────┐   TLS termination + Let's Encrypt
                  │    Traefik     │   (HTTP-01, acme.json on a PVC)
                  └───┬────────┬───┘
       Host(domain)   │        │   Host(grpc.domain), scheme h2c
                  ┌───▼────────▼───┐
                  │ hermione-server│   :8080 dashboard/SSE/WS
                  │   (1 replica)  │   :50051 gRPC ingest
                  └───────┬────────┘
                          │ VPC private network only
                  ┌───────▼────────┐
                  │ Managed Postgres│  no public endpoint
                  └────────────────┘

  Argo CD ── watches this repo ──▶ infra/charts/hermione
```

## Layout

| Path                      | What it is                                                     |
|---------------------------|----------------------------------------------------------------|
| `tofu/01-cluster`         | VPC, Kapsule cluster + pool, Managed PostgreSQL, reserved LB IP |
| `tofu/02-platform`        | Traefik, Argo CD, Hermione's namespace and Secret               |
| `charts/hermione`         | The Helm chart Argo CD deploys                                  |

### Why two stages

The `kubernetes` and `helm` providers must be *configured* before OpenTofu can
plan any resource belonging to them, and a cluster that does not exist yet
cannot supply an API endpoint or token. Splitting the root modules is what makes
`tofu plan` work at all; stage 02 reads stage 01's outputs through
`terraform_remote_state`.

## Prerequisites

- `tofu` ≥ 1.9, `helm`, `kubectl`
- Scaleway credentials in the environment. `scw init` writes them to
  `~/.config/scw/config.yaml`, which the provider reads directly. Verify with:

  ```bash
  scw account project list
  ```

  If that says "No credentials provided", run `scw init` before continuing.

## Deploy

```bash
cd infra
cp tofu/01-cluster/terraform.tfvars.example tofu/01-cluster/terraform.tfvars
$EDITOR tofu/01-cluster/terraform.tfvars

make init
make apply-cluster                          # ~10 min: cluster + database
echo 'acme_email = "you@example.com"' > tofu/02-platform/terraform.tfvars
make apply-platform                         # Traefik, Argo CD, the Application
make urls
```

```bash
make admin-password   # the seeded dashboard login
```

The dashboard is locked down from the first start — see below.

## DNS: today and later

You do not need a domain. Every hostname is derived from the reserved load
balancer IP through [sslip.io], which resolves `<anything>.<ip>.sslip.io` back to
`<ip>`:

| Purpose   | Hostname                    |
|-----------|-----------------------------|
| Dashboard | `<ip>.sslip.io`             |
| gRPC      | `grpc.<ip>.sslip.io`        |
| Argo CD   | `argocd.<ip>.sslip.io`      |

These are real public names, so Let's Encrypt's HTTP-01 challenge works and you
get genuine certificates.

When you have your own domain, point an `A` record (and a `grpc` and `argocd`
`A` record, or a wildcard) at the same IP — the IP is reserved separately from
the load balancer, so it survives Traefik being reinstalled — then set:

```hcl
# tofu/02-platform/terraform.tfvars
domain = "hermione.example.edu"
```

`make apply-platform` re-issues certificates and updates all three routes.

While iterating, set `acme_ca_server` to Let's Encrypt's staging directory so a
misconfiguration cannot exhaust the rate limit for a hostname.

## The admin account

Without an admin account Hermione runs in "open dev mode": the dashboard needs
no login and is scoped to a seeded `default` course. Since it streams students'
live terminals, that state must never be reachable from the internet.

So the server seeds an admin on its first start, from
`HERMIONE_BOOTSTRAP_ADMIN_USERNAME` / `HERMIONE_BOOTSTRAP_ADMIN_PASSWORD` in the
Secret, and grants it every existing course. This only happens while the admin
table is empty, so restarts and later password changes are never clobbered.

```bash
make admin-password   # username: admin, password: <generated>
```

Set `bootstrap_admin_username` / `bootstrap_admin_password` in
`terraform.tfvars` to choose them yourself.

Creating further admins and courses still goes through the provisioning API,
guarded by a separate token (`make admin-token`):

```bash
TOKEN=$(make -s admin-token)
HOST=$(tofu -chdir=tofu/02-platform output -raw hermione_url)

# Returns the course enrollment token students' recorders present.
curl -X POST "$HOST/api/admin/courses" \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"slug":"cs101","name":"CS 101"}'

curl -X POST "$HOST/api/admin/memberships" \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"username":"admin","courseSlug":"cs101"}'
```

Note that a course created after the first start is *not* auto-granted — the
seeding only runs once — so grant membership explicitly as above.

## Deliberate choices worth knowing

**One replica, `Recreate` strategy.** `hermione-server` keeps teacher login
sessions, the live terminal fan-out and the `/ws` broadcast hub in process
memory, and runs SeaORM migrations in-process at startup. A second replica would
drop cookies at random, show empty live terminals, and race the first pod's
migrations. Scaling out needs a shared session store and Postgres- or
Redis-backed pub/sub first.

**Timeouts are raised deliberately.** Almost all of Hermione's traffic is
long-lived streams. The Scaleway load balancer's default 10-minute tunnel
timeout and Traefik's default 180-second idle timeout would both sever live
terminals mid-lesson, so `scw-loadbalancer-timeout-tunnel` is 4h and the
`websecure` responding timeouts are disabled.

**The database has no public endpoint.** It is attached to the VPC private
network with IPAM enabled — which is what registers it in VPC DNS — and no
`load_balancer {}` block is declared, so it is reachable only from the cluster.
`prevent_destroy` is set on it.

**Secrets are not in git.** OpenTofu generates the admin token and identity
secret, reads the database DSN from the RDB resource, and writes them to a
Kubernetes Secret; the chart only references it by name. The trade-off is that
these values sit in OpenTofu state — keep it private, and move to Scaleway
Secret Manager with the External Secrets Operator once more than one person
operates this. Note that rotating a secret needs an explicit
`kubectl rollout restart`, since `envFrom` is only read at container start.

**Images come from `ghcr.io`.** `.github/workflows/release.yml` already builds
and pushes `ghcr.io/jvanbuel/hermione` on `v*` tags, so no Scaleway registry or
pull secret is involved. Set `image_tag` to a release tag rather than leaving it
at `latest`, so Argo CD has a meaningful diff.

## Optional configuration

```hcl
# tofu/02-platform/terraform.tfvars
anthropic_api_key = "sk-ant-..."   # enables the per-course AI teaching assistant
assistant_model   = "claude-opus-4-8"

# Enables verified student identity. Once set, Hermione ENFORCES it on ingest.
oidc_providers = jsonencode([
  { name = "github", kind = "github", clientId = "..." }
])
```

Omitted keys are left out of the Secret entirely rather than set to an empty
string, because `hermione-server` treats a present-but-empty
`HERMIONE_ANTHROPIC_API_KEY` as "assistant enabled" and then fails every call.

## Operating

```bash
make kubeconfig && export KUBECONFIG=$PWD/kubeconfig

kubectl -n hermione logs -f deploy/hermione
kubectl -n hermione rollout restart deploy/hermione

make argocd-password        # Argo CD UI login, username `admin`
```

Argo CD syncs automatically with prune and self-heal on, so changes to
`charts/hermione` on the tracked branch deploy on their own. Editing the live
resources by hand will be reverted.

## Cost

Roughly €45–55/month at the defaults: two `PRO2-XXS` nodes, one `LB-S` load
balancer, and a `db-dev-s` database. The Kapsule control plane is free. Check
Scaleway's pricing pages for current figures.

## Teardown

```bash
make destroy
```

The database has `prevent_destroy = true`; removing it is a deliberate two-step
(drop the lifecycle block, then destroy). `delete_additional_resources` is
`false`, so the cluster's load balancers and volumes are not swept up
automatically either.

[sslip.io]: https://sslip.io
