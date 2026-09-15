# AWS infrastructure

This Terraform root provisions a small deployment for SWARM, almost entirely
on AWS:

- one `t3.micro` Amazon Linux EC2 instance running Docker Compose, with a
  separate encrypted EBS volume for persistent SQLite and Caddy state;
- ECR repositories for the STUN/rendezvous and prompt web-app containers;
- a private S3 bucket behind CloudFront for the static landing site;
- Route 53 records (including for the relay, see below), an ACM certificate
  for CloudFront, and Caddy-managed HTTPS certificates for the two
  EC2-hosted web services; and
- Systems Manager access to the EC2 instance, so SSH is not exposed there.

The one exception is the fallback relay: it runs on a separate Oracle Cloud
Infrastructure (OCI) Always Free compute instance instead of AWS, to avoid a
second EC2/EBS bill (`relay-oracle.tf`). Its runtime contract is unchanged —
`RELAY_BIND`, `RELAY_PUBLIC_HOST`, and `RELAY_PUBLIC_PORT` — but it publishes
to OCI Container Registry (OCIR) rather than ECR, and it has its own DNS
record and public IP, separate from the EC2 app host.

The STUN container exposes the repository's current UDP reflector contract on
UDP 443 and fallback UDP 3478.

## Prerequisites

- Terraform 1.6 or newer
- AWS credentials with permission to create the documented resources
- a registered domain that can be delegated to Route 53, or an existing public
  Route 53 hosted zone
- a container image in each ECR repository whose service should start
- an Oracle Cloud account with Always Free eligibility in your home region,
  and an API signing key for a user with permission to manage compute,
  networking, and OCIR resources in the target compartment (see below)

## Deploy

```sh
cd deploy/terraform
cp terraform.tfvars.example terraform.tfvars
# Set domain_name, the oci_* variables (see "Oracle relay" below), and,
# when applicable, hosted_zone_id.
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

## Oracle relay

The relay is deployed separately from the rest of the stack, on an OCI
Always Free `VM.Standard.E2.1.Micro` instance, because it's the one
component that's cheaper to run on Oracle's free compute than on a second
AWS EC2 instance. To authenticate Terraform against OCI:

1. In the OCI console, note your tenancy OCID, your user's OCID, and your
   username (its login email).
2. Under the user's settings, add an API signing key. Save the generated
   private key locally (default expected path `~/.oci/oci_api_key.pem`) and
   copy the printed fingerprint.
3. Set `oci_tenancy_ocid`, `oci_user_ocid`, `oci_username`, `oci_fingerprint`,
   and `oci_region` (your tenancy's home region — Always Free compute is
   only available there) in `terraform.tfvars`. Leave `oci_compartment_ocid`
   unset to deploy into the tenancy's root compartment, or supply one.
4. If your `oci_region` isn't in the lookup table in `relay-oracle.tf`, also
   set `oci_registry_region_key` to the short region key OCI uses for OCIR
   hostnames.

`terraform apply` provisions the relay's own VCN, subnet, and security list;
a reserved public IP; a `relay.<domain_name>` Route 53 record pointing at it;
an OCIR repository; and an OCI auth token the instance uses to pull images.
Push the relay image with the `relay_registry_image` output as the tag and
`relay_registry_username`/`relay_registry_token` (sensitive) for
`docker login`:

```sh
terraform output -raw relay_registry_token | \
  docker login "$(terraform output -raw relay_registry_image | cut -d/ -f1)" \
  --username "$(terraform output -raw relay_registry_username)" --password-stdin
docker push "$(terraform output -raw relay_registry_image):latest"
```

A systemd timer on the instance reconciles the container every five minutes
using `relay_image_tag` (`latest` by default). SSH access is disabled unless
`relay_ssh_public_key` and a non-empty `relay_ssh_allowed_cidrs` are both
set; otherwise, use the OCI console's serial console for access.

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

The relay's `VM.Standard.E2.1.Micro` shape, its reserved public IP, and its
block and object storage usage are all part of Oracle's Always Free tier and
don't expire, but eligibility and allowances are still account- and
region-specific and can change. Review current OCI pricing and Always Free
terms before applying.
