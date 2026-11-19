export class LocaleOperationQueue {
  private tail: Promise<void> = Promise.resolve();

  run<T>(operation: () => Promise<T>): Promise<T> {
    const result = this.tail.then(operation, operation);
    this.tail = result.then(() => undefined, () => undefined);
    return result;
  }
}

export async function loadForFinalLocale(
  expectedLocale: string,
  resolveLocale: () => string,
  load: (locale: string) => Promise<void>,
): Promise<string> {
  await load(expectedLocale);
  const finalLocale = resolveLocale();
  if (finalLocale !== expectedLocale) await load(finalLocale);
  return finalLocale;
}
