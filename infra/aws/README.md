# Hermione on AWS (ECS Fargate)

One Fargate task behind an ALB, with RDS Postgres. Everything is in `main.tf`.

```
                        internet
                           │
                   ┌───────▼────────┐  ACM cert, TLS terminated here
                   │      ALB       │  :443, idle timeout 1h (SSE + WS)
                   └───┬────────┬───┘
   Host(hermione.…)    │        │   Host(grpc.hermione.…), GRPC target group
                   ┌───▼────────▼───┐
                   │ hermione-server│  :8080 dashboard/SSE/WS
                   │  1 Fargate task│  :50051 gRPC ingest
                   └───────┬────────┘
                           │ security group, no public route
                   ┌───────▼────────┐
                   │  RDS Postgres  │  db.t4g.micro, not publicly accessible
                   └────────────────┘
```

Uses the account's **default VPC** and its public subnets. The task gets a
public IP so it can pull the image from ghcr.io without a NAT gateway; its
security group only accepts traffic from the ALB.

## Deploy

```bash
tofu init
tofu apply
```

Takes roughly 15 minutes, almost all of it RDS. Then:

```bash
tofu output dashboard_url
tofu output -raw admin_password    # log in as `admin`
tofu output -raw admin_token       # for the /api/admin provisioning calls
```

## Before the first apply

**Tag a release.** `var.image` defaults to `ghcr.io/jvanbuel/hermione:latest`,
which the release workflow only pushes on a `v*` tag. The newest tag is v0.1.0
and main is 25 commits ahead of it, so `latest` is stale until you tag again.

To use the AI assistant, pass the key (it is otherwise left disabled):

```bash
tofu apply -var anthropic_api_key=sk-ant-...
```

## Deploys after the first

Nothing watches the registry. Push a new image tag, then:

```bash
aws ecs update-service --profile playground --region eu-west-1 \
  --cluster hermione --service hermione --force-new-deployment
```

## One task, on purpose

Teacher login sessions, the live terminal fan-out and the `/ws` hub are all in
process memory (`crates/server/src/auth.rs`, `crates/server/src/state.rs`). The
service is pinned to one task and deploys stop the old one before starting the
new one, so two are never live at once. That costs a few seconds of downtime
per deploy. Scaling out needs a shared session store and Postgres- or
Redis-backed pub/sub first.

## Course config

Point the course's `.devcontainer/devcontainer.json` at the outputs:

```jsonc
"HERMIONE_BACKEND": "https://grpc.hermione.playground.dataminded.cloud",
"HERMIONE_AUTH_URL": "https://hermione.playground.dataminded.cloud",
"HERMIONE_TOKEN": "<enrollment token from POST /api/admin/courses>"
```

Note there is no port: gRPC shares the ALB's :443 listener and is routed by
hostname, which also gets it through firewalls that only allow 443.

## State

State is local. Fine for one operator; move it to S3 before a second person
runs `apply`.
