# Deployment assets

SWARM's two server processes have intentionally different distribution
models:

- [`stun-server/`](stun-server/) contains the production container and Compose
  deployment for the public rendezvous service.
- [`terraform/`](terraform/) provisions the AWS EC2/container, ECR, S3,
  CloudFront, Route 53, and HTTPS infrastructure for a hosted deployment.
- `apps/server` is the end-user media server. Its Tauri GUI is packaged as a
  native desktop application rather than a container.

The repository root is the Docker build context because the STUN server uses
shared crates from the Cargo workspace.
