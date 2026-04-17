#import <Foundation/Foundation.h>

@protocol Drawable <NSObject>
- (void)draw;
- (CGFloat)area;
@end

@interface Rectangle : NSObject <Drawable>

@property (nonatomic, assign) CGFloat width;
@property (nonatomic, assign) CGFloat height;

- (instancetype)initWithWidth:(CGFloat)width height:(CGFloat)height;
- (CGFloat)perimeter;

@end

@implementation Rectangle

- (instancetype)initWithWidth:(CGFloat)width height:(CGFloat)height {
    self = [super init];
    if (self) {
        _width = width;
        _height = height;
    }
    return self;
}

- (void)draw {
    NSLog(@"Drawing rectangle %fx%f", self.width, self.height);
}

- (CGFloat)area {
    return self.width * self.height;
}

- (CGFloat)perimeter {
    return 2 * (self.width + self.height);
}

@end

CGFloat calculateDistance(CGFloat x1, CGFloat y1, CGFloat x2, CGFloat y2) {
    CGFloat dx = x2 - x1;
    CGFloat dy = y2 - y1;
    return sqrt(dx * dx + dy * dy);
}
