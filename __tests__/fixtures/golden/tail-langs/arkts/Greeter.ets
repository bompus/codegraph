import { formatName } from './util';
@Component
struct Greeter {
  @State name: string = 'world';
  private count: number = 0;
  build() {
    Column() {
      Text(this.greeting()).fontSize(20)
      Button('Greet').onClick(() => { this.bump(); })
    }
  }
  greeting(): string { return 'Hello, ' + formatName(this.name); }
  bump(): void { this.count += 1; }
}
export class GreeterService {
  greet(name: string): string { return formatName(name); }
}
