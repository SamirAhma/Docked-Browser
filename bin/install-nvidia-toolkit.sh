#!/usr/bin/env bash
# Install NVIDIA Container Toolkit so Docker can use the RTX GPU (--gpus all).
set -euo pipefail

if [[ "$(id -u)" -eq 0 ]]; then
  echo "Run this as your normal user (it will call sudo)."
  exit 1
fi

echo "==> Adding NVIDIA Container Toolkit apt repository"
curl -fsSL https://nvidia.github.io/libnvidia-container/gpgkey \
  | sudo gpg --dearmor -o /usr/share/keyrings/nvidia-container-toolkit-keyring.gpg

curl -fsSL https://nvidia.github.io/libnvidia-container/stable/deb/nvidia-container-toolkit.list \
  | sed 's#deb https://#deb [signed-by=/usr/share/keyrings/nvidia-container-toolkit-keyring.gpg] https://#g' \
  | sudo tee /etc/apt/sources.list.d/nvidia-container-toolkit.list >/dev/null

echo "==> Installing nvidia-container-toolkit"
sudo apt-get update
sudo apt-get install -y nvidia-container-toolkit

echo "==> Configuring Docker runtime"
sudo nvidia-ctk runtime configure --runtime=docker
sudo systemctl restart docker

echo "==> Verifying GPU in Docker"
docker run --rm --gpus all nvidia/cuda:12.6.0-base-ubuntu22.04 nvidia-smi

echo
echo "Done. Re-open a profile from Docked Browser (Close, then Open)."
