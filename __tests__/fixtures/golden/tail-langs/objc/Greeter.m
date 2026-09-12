#import "Greeter.h"
static NSString *const kDefaultName = @"world";
static NSString *capitalize(NSString *s) { return [s capitalizedString]; }
@implementation Greeter
- (instancetype)initWithName:(NSString *)name {
  if ((self = [super init])) { _name = [name copy]; }
  return self;
}
- (NSString *)greet {
  NSString *message = [NSString stringWithFormat:@"Hello, %@", capitalize(self.name)];
  [self.delegate greeterDidGreet:message];
  return message;
}
+ (Greeter *)defaultGreeter { return [[Greeter alloc] initWithName:kDefaultName]; }
@end
