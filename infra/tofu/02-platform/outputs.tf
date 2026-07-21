output "ingress_ip" {
  description = "Public IP all three hostnames resolve to. Point your own DNS here when you have it."
  value       = local.ingress_ip
}

output "hermione_url" {
  description = "Teacher dashboard."
  value       = "https://${local.app_host}"
}

output "hermione_grpc_backend" {
  description = "Value for the recorder's HERMIONE_BACKEND / --backend flag."
  value       = "https://${local.grpc_host}"
}

output "argocd_url" {
  description = "Argo CD UI. Username is `admin`."
  value       = "https://${local.argocd_host}"
}

output "argocd_initial_admin_password_command" {
  description = "Argo CD generates its own initial admin password; read it out of the cluster with this."
  value       = "kubectl -n argocd get secret argocd-initial-admin-secret -o jsonpath='{.data.password}' | base64 -d"
}

output "hermione_admin_username" {
  description = "Username of the admin account seeded at first start."
  value       = var.bootstrap_admin_username
}

output "hermione_admin_password" {
  description = "Password for the seeded admin account. Log in with it at /login, then change it."
  value       = local.bootstrap_admin_password
  sensitive   = true
}

output "hermione_admin_token" {
  description = "Bearer token for Hermione's provisioning API (/api/admin/*). Needed to create the first admin account — until one exists the dashboard is unauthenticated."
  value       = random_password.admin_token.result
  sensitive   = true
}
