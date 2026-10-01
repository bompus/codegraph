/** IPC side of native highlighting; the parent owns this process's deadline. */
import { tokenizeSource } from '../../extraction/syntax-tokens';
import type { Language } from '../../types';
interface TokenizeRequest { id: number; text: string; language: Language; }
process.on('message', async (msg: TokenizeRequest) => {
  let result: Awaited<ReturnType<typeof tokenizeSource>> = null;
  try { result = await tokenizeSource(msg.text, msg.language); } catch { result = null; }
  process.send?.({ id: msg.id, result });
});
process.on('disconnect', () => process.exit(0));
