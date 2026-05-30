// Frida bit-trace for Portal 2 (engine.dll) entity-prop decode.
//
// Goal: capture the engine's exact per-prop bit consumption for the first few
// EnterPVS entities (including the local player), so it can be diffed against
// demoscope's Rust decoder dump. The first prop where the bit consumption
// differs is the proto-4 decode bug.
//
// Usage (on Windows, Portal 2 + Frida installed):
//   1. Launch Portal 2 with -console.
//   2. frida portal2.exe -l p2_bittrace.js
//   3. In the game console:  playdemo youareamoron
//   4. Copy the [TRACE] lines from Frida's output and send them back.
//
// Addresses are engine.dll RVAs from the IDA session (imagebase 0x10000000):
//   ReadNextPropIndex  (CDeltaBitsReader::ReadNextPropIndex)  +0x173F70
//   CL_CopyNewEntity                                          +0x092940
//
// bf_read struct offsets (derived from the disassembly):
//   +0x14  m_nBitsAvail   (bits left in the cached 32-bit word, counts 32->0)
//   +0x18  m_pDataIn      (pointer to the NEXT 32-bit word to load)
// CDeltaBitsReader struct:
//   +0x00  m_pBuf  (bf_read*)
//   +0x08  m_iLastProp (int)

'use strict';

const RVA_READNEXTPROPINDEX = 0x173F70;
const RVA_COPYNEWENTITY      = 0x092940;

const BF_BITSAVAIL = 0x14;
const BF_DATAIN     = 0x18;
const CD_PBUF       = 0x00;
const CD_LASTPROP   = 0x08;

// Stop after this many entities so we don't flood (player is entity #1).
const MAX_ENTITIES = 6;
// Hard cap on ReadNextPropIndex calls logged, as a backstop.
const MAX_CALLS = 1200;

// Frida 17 made the Module API instance-based (Module.findBaseAddress was
// removed). Resolve the module a few ways for cross-version robustness.
function findEngineBase() {
  try { if (typeof Process.findModuleByName === 'function') {
    const m = Process.findModuleByName('engine.dll'); if (m) return m.base;
  }} catch (e) {}
  try { if (typeof Process.getModuleByName === 'function') {
    const m = Process.getModuleByName('engine.dll'); if (m) return m.base;
  }} catch (e) {}
  try { return Module.getBaseAddress('engine.dll'); } catch (e) {}
  try { return Module.findBaseAddress('engine.dll'); } catch (e) {}
  return null;
}

const base = findEngineBase();
if (base === null) {
  console.error('[TRACE] engine.dll not loaded yet - attach after the game is running.');
} else {
  console.log('[TRACE] engine.dll base = ' + base);

  let entityCount = -1;     // increments on each CopyNewEntity (0 = first)
  let callCount = 0;
  let done = false;

  // Absolute "virtual bit position" of a bf_read: (m_pDataIn * 8) - m_nBitsAvail.
  // Only diffs within one stream matter, so the (unknown) buffer-start offset
  // cancels out. m_pDataIn is a 32-bit pointer; *8 stays within JS safe ints.
  function bitPos(pBuf) {
    const bitsAvail = pBuf.add(BF_BITSAVAIL).readU32();
    const dataIn = pBuf.add(BF_DATAIN).readU32();   // pointer value as uint32
    return dataIn * 8 - bitsAvail;
  }

  // Marker per new entity. CL_CopyNewEntity is __usercall(a1@edi, a2, class, serial);
  // the class index is the 2nd stack argument. We log it best-effort plus an
  // ordinal so the player (#1) is identifiable regardless.
  Interceptor.attach(base.add(RVA_COPYNEWENTITY), {
    onEnter(args) {
      if (done) return;
      entityCount++;
      let cls = -1;
      try {
        // __usercall: return addr at [esp], stack args follow. a2 @ [esp+4],
        // class @ [esp+8], serial @ [esp+0xC]. (Order verified loosely; the
        // ordinal is the reliable key.)
        const sp = this.context.esp;
        cls = sp.add(8).readU32();
      } catch (e) {}
      console.log('[TRACE] ===== ENTITY #' + entityCount + ' class=' + cls + ' =====');
      if (entityCount >= MAX_ENTITIES) {
        done = true;
        console.log('[TRACE] reached MAX_ENTITIES, further calls suppressed.');
      }
    }
  });

  Interceptor.attach(base.add(RVA_READNEXTPROPINDEX), {
    onEnter(args) {
      if (done || callCount >= MAX_CALLS) return;
      this.skip = false;
      this.self = this.context.ecx;             // CDeltaBitsReader* (thiscall, ECX)
      this.pBuf = this.self.add(CD_PBUF).readPointer();
      this.lastProp = this.self.add(CD_LASTPROP).readS32();
      this.before = bitPos(this.pBuf);
    },
    onLeave(ret) {
      if (done || callCount >= MAX_CALLS || this.pBuf === undefined) return;
      const after = bitPos(this.pBuf);
      const idx = ret.toInt32();
      // stream = the CDeltaBitsReader instance (merge reads two: baseline + delta)
      console.log('[TRACE] rnpi stream=' + this.self +
                  ' last=' + this.lastProp +
                  ' -> idx=' + idx +
                  ' fiBits=' + (after - this.before) +
                  ' posBefore=' + this.before +
                  ' posAfter=' + after);
      callCount++;
    }
  });

  console.log('[TRACE] hooks installed. Run `playdemo youareamoron` in the console.');
}
