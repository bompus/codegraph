/**
 * `[super init]` goes past the class it is written in: to the superclass's
 * `init`, or — for an NSObject subclass — to no project method at all.
 * SDWebImage's NSObject subclasses sent `[super init]` to SDDiskCache's
 * `init` (their `@implementation` range was lost, so no hierarchy was read
 * and any `init` passed), and SDDiskCache's own `[super init]` came back to
 * itself.
 */
import { describe, it, expect, afterAll, beforeAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

let root = '';
let cg: CodeGraph;

beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-objc-super-'));
  const files: Record<string, string> = {
    'Compatibility.h': `#if DESKTOP
#define Image DesktopImage
#else
#define Image MobileImage
#endif
@interface DesktopImage : NSObject
- (id)initWithBytes:(id)bytes;
@end
@interface MobileImage : NSObject
@end
`,
    'Compatibility.m': `#import "Compatibility.h"
@implementation DesktopImage (Compatibility)
- (id)initWithBytes:(id)bytes { return self; }
@end
`,
    'Aliased.h': `#import "Compatibility.h"
@protocol Aliased
@end
@interface Aliased : Image
@end
`,
    'Aliased.m': `#import "Aliased.h"
@implementation Aliased
- (id)initWithBytes:(id)bytes { return [super initWithBytes:bytes]; }
@end
`,
    'Unrelated.h': `#define Image Other
@interface Unrelated : Image
@end
`,
    'Unrelated.m': `#import "Unrelated.h"
@implementation Unrelated
- (id)initWithBytes:(id)bytes { return [super initWithBytes:bytes]; }
@end
`,
    'Undefined.h': `#import "Compatibility.h"
#undef Image
@interface Undefined : Image
@end
`,
    'Undefined.m': `#import "Undefined.h"
@implementation Undefined
- (id)initWithBytes:(id)bytes { return [super initWithBytes:bytes]; }
@end
`,
    'Ambiguous.h': `#if DESKTOP
#define Choice LeftImage
#else
#define Choice RightImage
#endif
@interface LeftImage : NSObject
- (id)initWithBytes:(id)bytes;
@end
@interface RightBase : NSObject
- (id)initWithBytes:(id)bytes;
@end
@interface RightImage : RightBase
@end
@interface Ambiguous : Choice
@end
`,
    'Ambiguous.m': `#import "Ambiguous.h"
@implementation LeftImage
- (id)initWithBytes:(id)bytes { return self; }
@end
@implementation RightBase
- (id)initWithBytes:(id)bytes { return self; }
@end
@implementation Ambiguous
- (id)initWithBytes:(id)bytes { return [super initWithBytes:bytes]; }
@end
`,
    'Base.h': `#import <Foundation/Foundation.h>
@interface Base : NSObject
- (instancetype)init;
@end
`,
    'Base.m': `#import "Base.h"
@implementation Base
- (instancetype)init {
    self = [super init];
    return self;
}
@end
`,
    'Child.h': `#import "Base.h"
@interface Child : Base
@end
`,
    'Child.m': `#import "Child.h"
@implementation Child
- (instancetype)init {
    self = [super init];
    return self;
}
@end
`,
    'Both.m': `#import "Child.h"
@implementation Child
- (void)both { [self init]; [super init]; }
@end
`,
    'Other.m': `#import <Foundation/Foundation.h>
@interface Other : NSObject
@end
@implementation Other
- (instancetype)init {
    self = [super init];
    return self;
}
@end
`,
  };
  for (const [rel, content] of Object.entries(files)) {
    fs.writeFileSync(path.join(root, rel), content);
  }
  cg = await CodeGraph.init(root, { index: true });
});

afterAll(() => {
  cg?.close();
  if (root) fs.rmSync(root, { recursive: true, force: true });
});

/** `Owner::member` of every call edge out of a file. */
function callsFrom(file: string): string[] {
  const ids = cg.getNodesInFile(file).map((n) => n.id);
  return cg
    .getOutgoingEdgesFrom(ids)
    .filter((e) => e.kind === 'calls')
    .map((e) => cg.getNode(e.target)!.qualifiedName)
    .sort();
}

describe('Objective-C messages to super', () => {
  it('reach the superclass, never the sender itself or an unrelated class', () => {
    const ids = cg.getNodesInFile('Both.m').map(n => n.id);
    const sends = cg.getOutgoingEdgesFrom(ids).filter(e => e.kind === 'calls').sort((a, b) => a.column! - b.column!);
    expect(sends.map(e => cg.getNode(e.target)?.qualifiedName)).toEqual(['Child::init', 'Base::init']);
    expect(sends[0]!.column).toBeLessThan(sends[1]!.column!);
    expect(callsFrom('Child.m')).toEqual(['Base::init']);
    expect(callsFrom('Base.m')).toEqual([]);
    expect(callsFrom('Other.m')).toEqual([]);
    expect(callsFrom('Aliased.m')).toEqual(['DesktopImage::initWithBytes:']);
    expect(callsFrom('Unrelated.m')).toEqual([]);
    expect(callsFrom('Undefined.m')).toEqual([]);
    expect(callsFrom('Ambiguous.m')).toEqual([]);
  });
});
