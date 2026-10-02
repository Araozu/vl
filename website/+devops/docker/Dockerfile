FROM rust:1 AS catalog

WORKDIR /repo
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY src ./src
RUN cargo run --quiet --locked --bin vl -- stdlib > /stdlib.json

FROM node:26-alpine AS builder

WORKDIR /app

RUN npm i -g pnpm@11.13.0

COPY website/package.json website/pnpm-lock.yaml website/pnpm-workspace.yaml ./

RUN pnpm install --frozen-lockfile

COPY website/astro.config.mjs website/tsconfig.json ./
COPY website/public ./public
COPY website/src ./src
COPY website/scripts ./scripts
COPY --from=catalog /stdlib.json /stdlib.json
ENV VL_STDLIB_CATALOG=/stdlib.json

RUN pnpm build

FROM nginx:alpine

COPY website/+devops/docker/nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=builder /app/dist/ /usr/share/nginx/html/

EXPOSE 80
