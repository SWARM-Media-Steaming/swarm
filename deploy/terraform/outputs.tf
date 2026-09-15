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
  value = "${local.stun_domain}:${var.relay_port}"
}

output "ec2_public_ip" {
  value = aws_eip.app_host.public_ip
}

output "ecr_repository_urls" {
  description = "Push the stun-server, relay-server, and prompt-web images to these repositories."
  value       = { for name, repository in aws_ecr_repository.containers : name => repository.repository_url }
}
