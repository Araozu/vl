FROM node:26-alpine AS builder

WORKDIR /app

RUN npm i -g pnpm@11.13.0

COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./

RUN pnpm install --frozen-lockfile

COPY astro.config.mjs tsconfig.json ./
COPY public ./public
COPY src ./src

RUN pnpm build

FROM nginx:alpine

COPY +devops/docker/nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=builder /app/dist/ /usr/share/nginx/html/

EXPOSE 80
