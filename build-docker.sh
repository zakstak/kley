#!/usr/bin/env bash
set -e

IMAGE_NAME="kley-agent"

echo "Building Docker image: ${IMAGE_NAME}..."
docker build -t "${IMAGE_NAME}" .

echo ""
echo "Build complete! To run the agent interactively, use:"
echo ""
echo "    docker run -it -e OPENAI_API_KEY=\"your-api-key-here\" ${IMAGE_NAME}"
echo ""
echo "Note: If you have an active kley passphrase and stored credentials,"
echo "you can mount them into the container instead:"
echo "    docker run -it -v ~/.config/kley:/app/.config/kley ${IMAGE_NAME}"
