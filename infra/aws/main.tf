# ---------------------------------------------------------------------------
# Hermione on ECS Fargate.
#
# One task, one ALB, one managed Postgres. Deliberately uses the account's
# default VPC and its public subnets: the task needs egress to pull the image
# from ghcr.io, and a public subnet with a public IP gives it that without a
# NAT gateway. Nothing is reachable from the internet except the ALB.
# ---------------------------------------------------------------------------

terraform {
  required_version = ">= 1.9"
  required_providers {
    aws    = { source = "hashicorp/aws", version = "~> 5.60" }
    random = { source = "hashicorp/random", version = "~> 3.6" }
  }
}

provider "aws" {
  region  = var.region
  profile = var.profile

  default_tags {
    tags = { app = "hermione" }
  }
}

variable "region" { default = "eu-west-1" }
variable "profile" { default = "playground" }
variable "name" { default = "hermione" }

variable "zone_name" {
  description = "Route53 hosted zone that owns the hostnames below."
  default     = "playground.dataminded.cloud"
}

variable "host" {
  description = "Dashboard hostname. The gRPC endpoint is grpc.<host>."
  default     = "hermione.playground.dataminded.cloud"
}

variable "image" {
  description = "Server image. Tag a release first so this is not stale."
  default     = "ghcr.io/jvanbuel/hermione:latest"
}

variable "anthropic_api_key" {
  description = "Optional. Empty leaves the AI assistant disabled."
  default     = ""
  sensitive   = true
}

locals {
  grpc_host = "grpc.${var.host}"
}

# --- Existing network -------------------------------------------------------

data "aws_vpc" "default" { default = true }

data "aws_subnets" "default" {
  filter {
    name   = "vpc-id"
    values = [data.aws_vpc.default.id]
  }
  filter {
    name   = "default-for-az"
    values = ["true"]
  }
}

data "aws_route53_zone" "main" {
  name         = "${var.zone_name}."
  private_zone = false
}

# --- TLS --------------------------------------------------------------------

resource "aws_acm_certificate" "main" {
  domain_name               = var.host
  subject_alternative_names = [local.grpc_host]
  validation_method         = "DNS"

  lifecycle {
    create_before_destroy = true
  }
}

resource "aws_route53_record" "validation" {
  for_each = {
    for o in aws_acm_certificate.main.domain_validation_options : o.domain_name => o
  }

  zone_id         = data.aws_route53_zone.main.zone_id
  name            = each.value.resource_record_name
  type            = each.value.resource_record_type
  records         = [each.value.resource_record_value]
  ttl             = 60
  allow_overwrite = true
}

resource "aws_acm_certificate_validation" "main" {
  certificate_arn         = aws_acm_certificate.main.arn
  validation_record_fqdns = [for r in aws_route53_record.validation : r.fqdn]
}

# --- Security groups --------------------------------------------------------

resource "aws_security_group" "alb" {
  name   = "${var.name}-alb"
  vpc_id = data.aws_vpc.default.id

  ingress {
    description = "HTTPS: dashboard, SSE, WebSocket and gRPC (host-routed)"
    from_port   = 443
    to_port     = 443
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
  }

  ingress {
    description = "HTTP, redirected to HTTPS"
    from_port   = 80
    to_port     = 80
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

resource "aws_security_group" "task" {
  name   = "${var.name}-task"
  vpc_id = data.aws_vpc.default.id

  ingress {
    description     = "HTTP from the load balancer"
    from_port       = 8080
    to_port         = 8080
    protocol        = "tcp"
    security_groups = [aws_security_group.alb.id]
  }

  ingress {
    description     = "gRPC from the load balancer"
    from_port       = 50051
    to_port         = 50051
    protocol        = "tcp"
    security_groups = [aws_security_group.alb.id]
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

resource "aws_security_group" "db" {
  name   = "${var.name}-db"
  vpc_id = data.aws_vpc.default.id

  ingress {
    description     = "Postgres from the server task only"
    from_port       = 5432
    to_port         = 5432
    protocol        = "tcp"
    security_groups = [aws_security_group.task.id]
  }
}

# --- Database ---------------------------------------------------------------

resource "random_password" "db" {
  length  = 32
  special = false
}

resource "aws_db_subnet_group" "main" {
  name       = var.name
  subnet_ids = data.aws_subnets.default.ids
}

resource "aws_db_instance" "main" {
  identifier     = var.name
  engine         = "postgres"
  engine_version = "16.13" # pinned: "16" floats, and apply_immediately is on
  instance_class = "db.t4g.micro"

  allocated_storage = 20
  storage_type      = "gp3"
  storage_encrypted = true

  db_name  = "hermione"
  username = "hermione"
  password = random_password.db.result

  db_subnet_group_name   = aws_db_subnet_group.main.name
  vpc_security_group_ids = [aws_security_group.db.id]
  publicly_accessible    = false

  backup_retention_period = 1
  skip_final_snapshot     = true
  apply_immediately       = true
}

# --- Secrets ----------------------------------------------------------------

resource "random_password" "admin_token" {
  length  = 40
  special = false
}

resource "random_password" "bootstrap_admin" {
  length  = 20
  special = false
}

resource "aws_ssm_parameter" "database_url" {
  name  = "/${var.name}/HERMIONE_DATABASE_URL"
  type  = "SecureString"
  value = "postgres://hermione:${random_password.db.result}@${aws_db_instance.main.endpoint}/hermione"
}

resource "aws_ssm_parameter" "admin_token" {
  name  = "/${var.name}/HERMIONE_ADMIN_TOKEN"
  type  = "SecureString"
  value = random_password.admin_token.result
}

resource "aws_ssm_parameter" "bootstrap_admin_password" {
  name  = "/${var.name}/HERMIONE_BOOTSTRAP_ADMIN_PASSWORD"
  type  = "SecureString"
  value = random_password.bootstrap_admin.result
}

# Always created so the task definition can reference it unconditionally; an
# empty value leaves the assistant disabled.
resource "aws_ssm_parameter" "anthropic_api_key" {
  name  = "/${var.name}/HERMIONE_ANTHROPIC_API_KEY"
  type  = "SecureString"
  value = var.anthropic_api_key != "" ? var.anthropic_api_key : "unset"
}

# --- IAM --------------------------------------------------------------------

data "aws_iam_policy_document" "assume_ecs" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["ecs-tasks.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "execution" {
  name               = "${var.name}-execution"
  assume_role_policy = data.aws_iam_policy_document.assume_ecs.json
}

resource "aws_iam_role_policy_attachment" "execution" {
  role       = aws_iam_role.execution.name
  policy_arn = "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy"
}

data "aws_iam_policy_document" "read_secrets" {
  statement {
    actions = ["ssm:GetParameters"]
    resources = [
      aws_ssm_parameter.database_url.arn,
      aws_ssm_parameter.admin_token.arn,
      aws_ssm_parameter.bootstrap_admin_password.arn,
      aws_ssm_parameter.anthropic_api_key.arn,
    ]
  }
}

resource "aws_iam_role_policy" "read_secrets" {
  name   = "read-secrets"
  role   = aws_iam_role.execution.id
  policy = data.aws_iam_policy_document.read_secrets.json
}

resource "aws_iam_role" "task" {
  name               = "${var.name}-task"
  assume_role_policy = data.aws_iam_policy_document.assume_ecs.json
}

# --- Load balancer ----------------------------------------------------------

resource "aws_lb" "main" {
  name               = var.name
  load_balancer_type = "application"
  subnets            = data.aws_subnets.default.ids
  security_groups    = [aws_security_group.alb.id]

  # The dashboard holds an SSE stream and a WebSocket open. The 60s default
  # would cut both.
  idle_timeout = 3600
}

resource "aws_lb_target_group" "http" {
  name        = "${var.name}-http"
  port        = 8080
  protocol    = "HTTP"
  target_type = "ip"
  vpc_id      = data.aws_vpc.default.id

  deregistration_delay = 10

  health_check {
    path                = "/healthz"
    matcher             = "200"
    interval            = 15
    healthy_threshold   = 2
    unhealthy_threshold = 3
  }
}

resource "aws_lb_target_group" "grpc" {
  name             = "${var.name}-grpc"
  port             = 50051
  protocol         = "HTTP"
  protocol_version = "GRPC"
  target_type      = "ip"
  vpc_id           = data.aws_vpc.default.id

  deregistration_delay = 10

  # The server exposes no grpc.health.v1 service, so the probe comes back
  # UNIMPLEMENTED (12). Treating that as healthy proves the gRPC port is
  # answering, which is all this needs to know.
  health_check {
    path                = "/grpc.health.v1.Health/Check"
    matcher             = "0,12"
    interval            = 15
    healthy_threshold   = 2
    unhealthy_threshold = 3
  }
}

resource "aws_lb_listener" "http_redirect" {
  load_balancer_arn = aws_lb.main.arn
  port              = 80
  protocol          = "HTTP"

  default_action {
    type = "redirect"
    redirect {
      port        = "443"
      protocol    = "HTTPS"
      status_code = "HTTP_301"
    }
  }
}

resource "aws_lb_listener" "https" {
  load_balancer_arn = aws_lb.main.arn
  port              = 443
  protocol          = "HTTPS"
  ssl_policy        = "ELBSecurityPolicy-TLS13-1-2-2021-06"
  certificate_arn   = aws_acm_certificate_validation.main.certificate_arn

  default_action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.http.arn
  }
}

# gRPC shares the 443 listener and is picked out by hostname, so the recorder
# needs no non-standard port and no second certificate.
resource "aws_lb_listener_rule" "grpc" {
  listener_arn = aws_lb_listener.https.arn
  priority     = 10

  condition {
    host_header {
      values = [local.grpc_host]
    }
  }

  action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.grpc.arn
  }
}

# --- ECS --------------------------------------------------------------------

resource "aws_cloudwatch_log_group" "main" {
  name              = "/ecs/${var.name}"
  retention_in_days = 14
}

resource "aws_ecs_cluster" "main" {
  name = var.name
}

resource "aws_ecs_task_definition" "main" {
  family                   = var.name
  requires_compatibilities = ["FARGATE"]
  network_mode             = "awsvpc"
  cpu                      = 512
  memory                   = 1024
  execution_role_arn       = aws_iam_role.execution.arn
  task_role_arn            = aws_iam_role.task.arn

  runtime_platform {
    operating_system_family = "LINUX"
    cpu_architecture        = "X86_64"
  }

  container_definitions = jsonencode([{
    name      = "server"
    image     = var.image
    essential = true
    # The image sets no USER, so without this the task runs as root. Same uid
    # the Helm chart pins (infra/charts/hermione/values.yaml).
    user = "65532:65532"

    portMappings = [
      { containerPort = 8080, protocol = "tcp" },
      { containerPort = 50051, protocol = "tcp" },
    ]

    environment = [
      { name = "HERMIONE_HTTP_ADDR", value = "0.0.0.0:8080" },
      { name = "HERMIONE_GRPC_ADDR", value = "0.0.0.0:50051" },
      { name = "HERMIONE_BOOTSTRAP_ADMIN_USERNAME", value = "admin" },
      { name = "RUST_LOG", value = "hermione_server=info,tower_http=info" },
    ]

    secrets = concat([
      { name = "HERMIONE_DATABASE_URL", valueFrom = aws_ssm_parameter.database_url.arn },
      { name = "HERMIONE_ADMIN_TOKEN", valueFrom = aws_ssm_parameter.admin_token.arn },
      { name = "HERMIONE_BOOTSTRAP_ADMIN_PASSWORD", valueFrom = aws_ssm_parameter.bootstrap_admin_password.arn },
      ], var.anthropic_api_key != "" ? [
      # Only when a real key was given: the parameter otherwise holds the
      # placeholder, which the server would take for a live key.
      { name = "HERMIONE_ANTHROPIC_API_KEY", valueFrom = aws_ssm_parameter.anthropic_api_key.arn },
    ] : [])

    readonlyRootFilesystem = true

    logConfiguration = {
      logDriver = "awslogs"
      options = {
        "awslogs-group"         = aws_cloudwatch_log_group.main.name
        "awslogs-region"        = var.region
        "awslogs-stream-prefix" = "server"
      }
    }
  }])
}

# ---------------------------------------------------------------------------
# Exactly one task, always.
#
# Teacher login sessions, the live terminal fan-out and the /ws broadcast hub
# all live in process memory (crates/server/src/{auth,state}.rs). Two tasks
# would drop teachers' cookies at random, show empty live terminals, and race
# each other running the SeaORM migrations at startup. min 0% / max 100% makes
# a deploy stop the old task before starting the new one, which costs a few
# seconds of downtime and is the point.
# ---------------------------------------------------------------------------
resource "aws_ecs_service" "main" {
  name            = var.name
  cluster         = aws_ecs_cluster.main.id
  task_definition = aws_ecs_task_definition.main.arn
  launch_type     = "FARGATE"
  desired_count   = 1

  deployment_minimum_healthy_percent = 0
  deployment_maximum_percent         = 100

  deployment_circuit_breaker {
    enable   = true
    rollback = true
  }

  # The chart allows 150s for a first migration against an empty database
  # (startupProbe, 30 x 5s). With min 0% there is no second task to fall back
  # on, so this is deliberately more generous than that.
  health_check_grace_period_seconds = 300

  network_configuration {
    subnets = data.aws_subnets.default.ids
    # Public subnets with no NAT gateway, so the task needs its own address to
    # reach ghcr.io. The security group is what keeps it private.
    assign_public_ip = true
    security_groups  = [aws_security_group.task.id]
  }

  load_balancer {
    target_group_arn = aws_lb_target_group.http.arn
    container_name   = "server"
    container_port   = 8080
  }

  load_balancer {
    target_group_arn = aws_lb_target_group.grpc.arn
    container_name   = "server"
    container_port   = 50051
  }

  depends_on = [aws_lb_listener.https, aws_lb_listener_rule.grpc]
}

# --- DNS --------------------------------------------------------------------

resource "aws_route53_record" "dashboard" {
  zone_id = data.aws_route53_zone.main.zone_id
  name    = var.host
  type    = "A"

  alias {
    name                   = aws_lb.main.dns_name
    zone_id                = aws_lb.main.zone_id
    evaluate_target_health = false
  }
}

resource "aws_route53_record" "grpc" {
  zone_id = data.aws_route53_zone.main.zone_id
  name    = local.grpc_host
  type    = "A"

  alias {
    name                   = aws_lb.main.dns_name
    zone_id                = aws_lb.main.zone_id
    evaluate_target_health = false
  }
}

# --- Outputs ----------------------------------------------------------------

output "dashboard_url" { value = "https://${var.host}" }
output "grpc_backend" { value = "https://${local.grpc_host}" }

output "admin_username" { value = "admin" }

output "admin_password" {
  value     = random_password.bootstrap_admin.result
  sensitive = true
}

output "admin_token" {
  value     = random_password.admin_token.result
  sensitive = true
}
