output "route53_name_servers" {
  description = "Registrar delegation targets. Empty when an existing hosted zone is supplied."
  value       = var.hosted_zone_id == null ? aws_route53_zone.this[0].name_servers : []
}

output "static_site_bucket" {
  description = "Upload the landing-page files to this private bucket."
  value       = aws_s3_bucket.static_site.id
}

output "static_site_url" {
  value = "https://${var.domain_name}"
}

output "cloudfront_distribution_id" {
  description = "Distribution to invalidate after uploading a static-site release."
  value       = aws_cloudfront_distribution.static_site.id
}

output "stun_url" {
  value = "https://${local.stun_domain}"
}

output "web_app_url" {
  value = "https://${local.web_app_domain}"
}

output "relay_endpoint" {
  description = "The Oracle-hosted fallback relay's address."
  value       = "${local.relay_domain}:${var.relay_port}"
}

output "ec2_public_ip" {
  value = aws_eip.app_host.public_ip
}

output "ecr_repository_urls" {
  description = "Push the stun-server and prompt-web images to these repositories."
  value       = { for name, repository in aws_ecr_repository.containers : name => repository.repository_url }
}

output "relay_public_ip" {
  description = "Reserved public IP of the Oracle Cloud relay instance."
  value       = oci_core_public_ip.relay.ip_address
}

output "relay_registry_image" {
  description = "Push the relay-server image to this OCI Container Registry repository (tag it with relay_image_tag)."
  value       = "${local.relay_registry_host}/${local.relay_registry_repository}"
}

output "relay_registry_username" {
  description = "Docker login username for the OCIR relay repository."
  value       = "${data.oci_objectstorage_namespace.relay.namespace}/${var.oci_username}"
}

output "relay_registry_token" {
  description = "Docker login password (OCI auth token) for the OCIR relay repository. Also used by the relay instance itself to pull images."
  value       = oci_identity_auth_token.relay_registry.token
  sensitive   = true
}
