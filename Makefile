.DEFAULT_GOAL := help

.PHONY: help setup dev web build test check bundle-mac

help: ## 显示可用命令
	@printf '用法：make [目标]\n\n'
	@awk -F ':.*## ' '/^[a-z][a-z0-9-]*:.*## / { printf "  %-12s %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

setup: ## 安装前端和追踪依赖，准备追踪模型
	npm ci
	sh tracker/setup.sh

dev: ## 启动 Tauri 桌面应用的开发模式
	npm run tauri -- dev

web: ## 启动前端开发服务器
	npm run dev

build: ## 检查 TypeScript 类型并构建前端
	npm run build

test: ## 运行 Rust 测试
	cargo test --manifest-path src-tauri/Cargo.toml

check: ## 检查 Rust 格式和追踪脚本语法
	cargo fmt --manifest-path src-tauri/Cargo.toml --check
	sh -n tracker/setup.sh
	sh -n tracker/bundle.sh

bundle-mac: ## 打包 macOS 应用
	npm run bundle:mac
