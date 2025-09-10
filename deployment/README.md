# Kubernetes Deployment Guide

This guide explains how to deploy the Nostr Push Service to Kubernetes with support for multiple Firebase projects.

## Overview

The service supports multiple apps (nostrpushdemo and universes), each with its own Firebase project and credentials. K8s secrets are mounted as actual files in the container.

## How K8s Secrets Work

When you create a K8s secret from a file, Kubernetes:
1. Base64 encodes the file content when storing it
2. Mounts it as an actual file in the container (automatically decoded)
3. The application reads it as a normal JSON file

## Credential Management

The deployment mounts Firebase service account JSON files from secrets:

```yaml
# deployment.yaml
env:
  - name: NOSTR_PUSH__APPS__NOSTRPUSHDEMO__CREDENTIALS_PATH
    value: /app/secrets/firebase-nostrpushdemo.json
  - name: NOSTR_PUSH__APPS__UNIVERSES__CREDENTIALS_PATH
    value: /app/secrets/firebase-universes.json

volumes:
  - name: google-application-credentials
    secret:
      secretName: nostr-push-secret
      items:
        - key: firebase-nostrpushdemo-credentials
          path: firebase-nostrpushdemo.json
        - key: firebase-universes-credentials
          path: firebase-universes.json
```

## Creating Secrets

```bash
# Create secret with your service account JSON files
kubectl create secret generic nostr-push-secret \
  --from-file=firebase-nostrpushdemo-credentials=./firebase-service-account-nostrpushdemo.json \
  --from-file=firebase-universes-credentials=./firebase-service-account-universes.json \
  --from-literal=app-nip29-relay-private-key="your_private_key_hex" \
  --from-literal=redis-connection-string="redis://your-redis:6379" \
  --namespace=nostr-push
```

Note: The `--from-file` flag automatically handles the base64 encoding. The files will be available inside the container at the paths specified in the deployment.

## Using SealedSecrets (Recommended for GitOps)

For GitOps workflows, use SealedSecrets to safely store encrypted secrets in Git:

```bash
# Install sealed-secrets controller (once per cluster)
kubectl apply -f https://github.com/bitnami-labs/sealed-secrets/releases/download/v0.18.0/controller.yaml

# Create a sealed secret
kubectl create secret generic nostr-push-secret --dry-run=client -o yaml \
  --from-file=firebase-nostrpushdemo-credentials=./firebase-service-account-nostrpushdemo.json \
  --from-file=firebase-universes-credentials=./firebase-service-account-universes.json \
  --from-literal=app-nip29-relay-private-key="your_key" \
  --from-literal=redis-connection-string="redis://redis:6379" \
  | kubeseal -o yaml > sealed-secret.yaml

# Commit sealed-secret.yaml to Git (safe - it's encrypted)
git add sealed-secret.yaml
git commit -m "Update sealed secrets"
```

## Deployment Steps

1. **Prepare Firebase Credentials**
   - Download service account JSON for `plur-push-local` (nostrpushdemo)
   - Download service account JSON for `universes-2bc44` (universes)

2. **Create Namespace**
   ```bash
   kubectl create namespace nostr-push
   ```

3. **Create Secrets** (choose one approach from above)

4. **Deploy with Helm**
   ```bash
   cd deployment/nostr-push
   helm install nostr-push . --namespace nostr-push
   ```

5. **Verify Deployment**
   ```bash
   kubectl get pods -n nostr-push
   kubectl logs -n nostr-push deployment/nostr-push-deployment
   ```

## Environment Variables Reference

### Required
- `NOSTR_PUSH__SERVICE__PRIVATE_KEY_HEX` - Service private key for NIP-44
- `REDIS_URL` - Redis connection string

### Firebase Credentials (per app)
Each app needs one of these:
- `NOSTR_PUSH__APPS__<APPNAME>__CREDENTIALS_PATH` - Path to JSON file
- `NOSTR_PUSH__APPS__<APPNAME>__FCM_CREDENTIALS_BASE64` - Base64-encoded JSON

Where `<APPNAME>` is:
- `NOSTRPUSHDEMO` for the demo app
- `UNIVERSES` for the Universes app

## Troubleshooting

### Check Logs
```bash
kubectl logs -n nostr-push deployment/nostr-push-deployment
```

### Verify Secrets
```bash
kubectl get secrets -n nostr-push
kubectl describe secret nostr-push-secret -n nostr-push
```

### Test Credentials
The service logs will show which credentials were loaded:
- "Set credentials path for app universes: /app/secrets/firebase-universes.json"
- "Set base64 credentials for app universes"
- "Using default file for app universes: ./firebase-service-account-universes.json"

### Common Issues

1. **"No FCM credentials found for app"**
   - Check secret keys match expected names
   - Verify files are properly base64 encoded
   - Check environment variable names are uppercase

2. **"Failed to initialize FCM client"**
   - Verify service account has FCM permissions
   - Check Firebase project ID matches in settings.yaml
   - Ensure service account is from correct project

3. **Multiple Apps Not Working**
   - Each app needs its own service account
   - Firebase project IDs must be correct in settings.yaml
   - Check logs to see which credentials loaded