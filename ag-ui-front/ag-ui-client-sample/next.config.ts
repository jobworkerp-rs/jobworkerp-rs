import type { NextConfig } from "next";

const backendUrl = process.env.AG_UI_BASE_URL || process.env.NEXT_PUBLIC_AG_UI_BASE_URL || "http://localhost:8080";
console.log("[Next.Config] Usage AG-UI Backend URL:", backendUrl);

const nextConfig: NextConfig = {
  async rewrites() {
    return [
      {
        source: "/ag-ui/:path*",
        destination: `${backendUrl}/ag-ui/:path*`,
      },
    ];
  },
};

export default nextConfig;
