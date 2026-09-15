# AWS infrastructure

This Terraform root provisions a small AWS deployment for SWARM:

- one `t3.micro` Amazon Linux EC2 instance running Docker Compose, with a
  separate encrypted EBS volume for persistent SQLite and Caddy state;
- ECR repositories for the STUN/rendezvous, future fallback relay, and prompt
  web-app containers;
- a private S3 bucket behind CloudFront for the static landing site;
- Route 53 records, an ACM certificate for CloudFront, and Caddy-managed HTTPS
  certificates for the two EC2-hosted web services; and
- Systems Manager access to the instance, so SSH is not exposed.

The STUN container exposes the repository's current UDP reflector contract on
UDP 443 and fallback UDP 3478. The requested media relay does not exist in the
application yet, so the infrastructure reserves a separate container and
TCP/UDP port. Its runtime contract is `RELAY_BIND`, `RELAY_PUBLIC_HOST`, and
`RELAY_PUBLIC_PORT`.

## Prerequisites

- Terraform 1.6 or newer
- AWS credentials with permission to create the documented resources
- a registered domain that can be delegated to Route 53, or an existing public
  Route 53 hosted zone
- a container image in each ECR repository whose service should start

## Deploy

```sh
cd deploy/terraform
cp terraform.tfvars.example terraform.tfvars
# Set domain_name and, when applicable, hosted_zone_id.
terraform init
terraform plan
terraform apply
```

When creating the hosted zone, DNS must be delegated before ACM can validate
the certificate. Bootstrap the zone first, update the registrar to use the
output, wait for delegation to resolve, and then apply the complete stack:

```sh
terraform apply -target=aws_route53_zone.this
terraform output route53_name_servers
# Update the registrar, then verify the new NS records resolve.
terraform apply
```

Build and push each container using the URLs in `ecr_repository_urls`. All
three use `ecr_image_tag` (`latest` by default). A systemd timer checks every
five minutes and deploys each available image independently. To deploy a later
image with the same tag immediately, start a Systems Manager session and run:

```sh
sudo systemctl restart swarm-containers
```

Upload the future HTML/CSS/JavaScript landing site and invalidate the CDN:

```sh
aws s3 sync ./site "s3://$(terraform output -raw static_site_bucket)" --delete
aws cloudfront create-invalidation \
  --distribution-id "$(terraform output -raw cloudfront_distribution_id)" \
  --paths '/*'
```

The prompt web container must listen on port 8080. The existing STUN image
already uses port 8080 and internal reflector ports 9443/UDP and 3478/UDP.
Back up the persistent data volume with EBS snapshots. It survives an EC2
replacement but is deleted by `terraform destroy`.

## State and cost

Terraform state can contain infrastructure metadata and must not be committed.
For team use, configure an encrypted remote backend before the first production
apply.

The defaults are selected to fit the EC2 and EBS free-tier allowances available
to qualifying accounts, and CloudFront/S3/ECR usage can remain within their
allowances at low traffic. AWS free-tier eligibility and limits vary by account
and change over time. Route 53 hosted zones, the required public IPv4 address,
traffic, storage, and requests can incur charges. Review the Terraform plan and
current AWS pricing, and configure AWS Budgets before applying.
