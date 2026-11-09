import { createClient } from '@connectrpc/connect';
import { createConnectTransport } from '@connectrpc/connect-web';
import { HealthService } from './gen/arcdos/account/v1/health_pb';

export async function checkHealth(baseUrl: string, signal: AbortSignal): Promise<boolean> {
  const transport = createConnectTransport({ baseUrl, useBinaryFormat: true });
  const client = createClient(HealthService, transport);
  const response = await client.check({}, { signal, timeoutMs: 5_000 });
  return response.ready;
}
