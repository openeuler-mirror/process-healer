#!/bin/bash

set -e

ROOT_DIR=$(cd "$(dirname "$0")/.." && pwd)
HEALER_ROOT="${ROOT_DIR}/healer-demo"
RUN_DIR="${HEALER_ROOT}/run"
LOG_DIR="${HEALER_ROOT}/logs"

echo "正在为您准备 healer 快速上手目录:"
echo "  ROOT: ${HEALER_ROOT}"
echo "  RUN : ${RUN_DIR}"
echo "  LOG : ${LOG_DIR}"
echo "------------------------------------------"

mkdir -p "${RUN_DIR}" "${LOG_DIR}"

echo "目录已创建："
ls -ld "${HEALER_ROOT}" "${RUN_DIR}" "${LOG_DIR}"
