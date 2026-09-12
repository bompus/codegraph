#import <Foundation/Foundation.h>
@protocol GreeterDelegate <NSObject>
- (void)greeterDidGreet:(NSString *)message;
@end
@interface Greeter : NSObject
@property (nonatomic, weak) id<GreeterDelegate> delegate;
@property (nonatomic, copy) NSString *name;
- (instancetype)initWithName:(NSString *)name;
- (NSString *)greet;
+ (Greeter *)defaultGreeter;
@end
