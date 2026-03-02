#!/bin/bash

# Deploy script for application
deploy() {
    local env="$1"
    local version="$2"

    echo "Deploying version $version to $env"
    build_artifacts "$version"
    upload_package "$env"
}

# Build artifacts for release
build_artifacts() {
    local version="$1"
    mkdir -p dist
    tar czf "dist/app-${version}.tar.gz" src/
}

# Upload package to target environment
upload_package() {
    local env="$1"
    scp "dist/app-*.tar.gz" "deploy@${env}.example.com:/opt/app/"
}

# Check if service is running
health_check() {
    local host="$1"
    local retries=5

    for i in $(seq 1 $retries); do
        if curl -sf "http://${host}:8080/health" > /dev/null; then
            echo "Service healthy"
            return 0
        fi
        sleep 2
    done
    return 1
}
