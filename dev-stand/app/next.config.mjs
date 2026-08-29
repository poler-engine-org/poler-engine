/** @type {import('next').NextConfig} */
const nextConfig = {
  async rewrites() {
    // API-плоскость релея превью (скринкаст/ввод/статус) — он на 127.0.0.1:3100
    return [
      { source: '/api/:path*', destination: 'http://127.0.0.1:3100/:path*' },
    ];
  },
};

export default nextConfig;
