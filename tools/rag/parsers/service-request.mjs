const unavailableCodes = new Set(['ECONNREFUSED', 'ENETUNREACH', 'EHOSTUNREACH', 'ENOTFOUND', 'EAI_AGAIN', 'UND_ERR_CONNECT_TIMEOUT', 'ETIMEDOUT']);

export class ServiceUnavailable extends Error {}

export async function fetchService(service, baseUrl, route, options, signal) {
  const timeout = AbortSignal.timeout(options.body ? 180_000 : 10_000);
  try {
    return await fetch(`${baseUrl.replace(/\/+$/u, '')}/${route}`, {
      ...options,
      signal: signal ? AbortSignal.any([signal, timeout]) : timeout,
    });
  } catch (error) {
    signal?.throwIfAborted();
    const Failure = connectionUnavailable(error.cause) ? ServiceUnavailable : Error;
    throw new Failure(`Could not reach ${service} at ${baseUrl}: ${error.message}`, { cause: error });
  }
}

function connectionUnavailable(error) {
  if (error instanceof AggregateError) return error.errors.length > 0 && error.errors.every(connectionUnavailable);
  return unavailableCodes.has(error?.code);
}
